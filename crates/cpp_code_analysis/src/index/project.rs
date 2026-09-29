//! Many files at once: the summaries, the edges between them, and the queries that need both.
//!
//! [`crate::index`] turns one file into a [`FileSummary`]. This module is what holds a project's worth of them
//! and answers the questions that cross a file boundary — which is the whole reason an index exists, and the
//! layer calls `resolve`.
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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use super::names::{NameIndex, Posting};
use crate::guard::Visibility;
use crate::include::graph::Marked;
use crate::file::paths::normalize_path;
use crate::summary::{DeclFact, DeclKind, FactGuard, FileSummary, MacroFact};
use crate::preprocess::directive::IncludeForm;
use crate::symbol::{Known, UnknownReason};

/// How many files a visibility walk will cross before giving up.
///
/// A backstop rather than a policy: real include graphs are shallow (the walker's own limit is
/// [`crate::MAX_INCLUDE_DEPTH`]), and a corpus that exceeds this is one whose summaries disagree with its
/// includes — which the visited set already handles. The number is here so that the *round* count of a
/// pathological graph cannot become an unbounded amount of work on a keystroke.
const MAX_VISIBILITY_DEPTH: usize = 128;

/// **Where an `#include` points** — the target a reader asks about by pointing at the header's name.
///
/// The fourth answer a "go to definition" can give, beside a declaration, an overload set and a macro, and the one
/// it was missing: nothing in the file *declares* `vector`, so every question the jump is built out of — the scope
/// walk, the index by name, the macro table — answers nothing at all for `#include <vector>`, and a reader who
/// ctrl-clicks it is told there is no definition to go to.
///
/// The name is the spelling between the delimiters (`vector`, `sys/types.h`), which is what the resolver searched
/// for and what a reader wrote; the delimiters are the *form*, which is why [`HeaderTarget::form`] is separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderTarget {
    /// The spelling between the delimiters, and — for [`IncludeForm::Macro`] — the macro's name.
    pub spelling: String,
    pub form: IncludeForm,
    /// The directive, as a range in the file **being edited**, which is what a client highlights.
    ///
    /// The whole `#include <vector>` rather than the name inside it, and that is the honest range: a jump to a
    /// header lands at the top of a file whose contents have nothing to do with this one, and highlighting the
    /// directive that asked for it is what tells the reader *why* they are looking at it.
    pub range: cpp_parser::SourceRange,
    /// The file it resolved to, as a path a client can open.
    pub resolved: PathBuf,
}

/// The `#include` whose **directive** covers `offset`, if there is one and it resolved.
///
/// # What it answers, and what it deliberately does not
///
/// * `Yes(target)` — the cursor is on an `#include` line and the resolver found the file. The spelling is the one
///   written, which is what a client shows; the path is where the file is, which is what it opens.
/// * `Unknown(NotDeclaredHere)` — the line is an `#include` and **nothing was found** for it: a header outside every
///   search path, or one whose file was deleted since. Not `No`, and the distinction is the one the rest of this
///   module keeps: "the index cannot see it" is a different claim from "there is no such thing".
/// * `Unknown(UnparsableName)` — the cursor is not on an `#include` at all. Either nothing is here, or what is here
///   is a different kind of name (a macro include's target is [`ProjectIndex::macro_definition`]'s question).
/// * `Unknown(ConditionalCompilation)` — the `#include` is inside an `#if` this layer cannot evaluate, so whether
///   the file is part of the translation unit is not known.
///
/// The directive's own range is what the cursor is matched against rather than the header-name token, and the two
/// differ on purpose: a reader who points at `#include` or at the `<` means the same thing as one who points at
/// `vector`, and the tree has no node for the second half of the line when the lexer never folded it.
pub fn header_at(
    index: &ProjectIndex,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<HeaderTarget> {
    // The tree answers *which line*, because only the tree knows where the cursor is; the summary answers *where it
    // points*, because resolution happened when the file was read and re-running the search here would be a second
    // implementation of the resolver.
    let Some((spelling, name_range)) = crate::sema::resolve::header_name_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let Some(summary) = index.summary(path) else {
        // The file's own reading is not in the index, so nothing here knows where its includes resolved. The
        // spelling is what the caller pointed at, and saying it is better than an empty reason.
        return Known::Unknown(UnknownReason::UnresolvedInclude(Box::from(
            spelling.as_str(),
        )));
    };

    // The fact whose **directive** covers the name: `range` is the whole `#include …` line, and the name sits
    // inside it. A file has few includes, so this is a scan of a handful of ranges.
    let Some(fact) = summary.includes.iter().find(|include| {
        include.range.start_offset <= name_range.start_offset
            && include.range.end_offset() >= name_range.end_offset()
    }) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    match fact.guard {
        FactGuard::Unconditional => {}
        FactGuard::Region(_) => {
            // The directive is inside an `#if`. Whether it is part of the translation unit is a question this
            // layer answers elsewhere (`index::environment::visibility_at`) and this query does not ask, so the
            // honest answer is the one every other conditional answer in this module gives.
            return Known::Unknown(UnknownReason::ConditionalCompilation);
        }
    }

    // **A macro include's target is a macro.** `#include HEADER` names a name, and what it expands to is the
    // preprocessor's answer — which is the same question `#define` answers, so it goes to the same place rather
    // than being guessed at here.
    if fact.form == IncludeForm::Macro {
        return Known::Unknown(UnknownReason::UnresolvedInclude(Box::from(
            fact.spelling.as_str(),
        )));
    }

    match &fact.resolved {
        Some(resolved) => Known::Yes(HeaderTarget {
            spelling: fact.spelling.clone(),
            form: fact.form,
            range: fact.range,
            resolved: resolved.clone(),
        }),
        // **The search ran and found nothing** — the header is on no include path this project configured, or the
        // file it named has been deleted since. `UnresolvedInclude` is exactly this reason, and it is the one the
        // rest of the crate already uses for a declaration reachable only through an include nobody could find.
        None => Known::Unknown(UnknownReason::UnresolvedInclude(Box::from(
            fact.spelling.as_str(),
        ))),
    }
}

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
    match definitions_across_files(index, scopes, root, path, offset) {
        Known::Yes(mut found) if found.found.len() == 1 => Known::Yes(found.found.remove(0)),
        Known::Yes(_) => Known::Unknown(UnknownReason::Ambiguous(Box::from(
            // The spelling the cursor wrote, read back from the tree rather than taken from the list: a reader
            // asking "which `find` is this" wants the name they pointed at, not the qualified name of whichever
            // overload happens to sort first.
            crate::sema::resolve::qualified_name_at(root, offset)
                .map(|(written, _)| written)
                .unwrap_or_default(),
        ))),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::No,
    }
}

/// **[`member_across_files`] as a list** — every declaration of the member, in the class the object's type names.
///
/// # Why this is a different question from "where is this name declared"
///
/// A member written after a `.` is not looked up among the names in scope: it is looked up **in the type of the
/// object**, and the difference is measured on one real file — `line.empty()` asked by name answers with **twelve**
/// declarations of `empty` (every `empty` in the standard library, from classes the reader never mentioned) while
/// asked of the object's type it is `basic_string`'s two. The name query is a heuristic that happens to work often
/// enough to be worth keeping as a fallback; this is the reading.
///
/// The set is [`members_of`]'s, filtered by name: that walk already follows bases and already hides what a nearer
/// level hides, which is exactly the set of declarations a name in this position can refer to — and it is the same
/// walk the completion after `.` shows, so a jump and the list a reader was just looking at cannot disagree.
pub fn member_definitions_across_files(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinitions> {
    let Some(access) = crate::sema::resolve::member_access_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    if access.member.is_empty() {
        return Known::Unknown(UnknownReason::UnparsableName);
    }

    let Known::Yes((written, _)) = type_of_expression(index, scopes, root, path, &access.object, 0)
    else {
        let Known::Unknown(reason) =
            type_of_expression(index, scopes, root, path, &access.object, 0)
        else {
            unreachable!("the first match established that this is an `Unknown`")
        };
        return Known::Unknown(reason);
    };

    let class = base_type_name(&written);
    if class.is_empty() {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    let members = match members_of(index, scopes, root, path, class) {
        Known::Yes(members) => members,
        Known::Unknown(reason) => return Known::Unknown(reason),
        Known::No => return Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    };

    let found: Vec<ProjectDefinition> = members
        .members
        .iter()
        .filter(|member| member.fact.name == access.member)
        .map(|member| ProjectDefinition {
            file: member.file.clone(),
            fact: member.fact.clone(),
        })
        .collect();

    if found.is_empty() {
        // **Not a definite no**: the class may be a template this layer does not instantiate, or inherit from one
        // it could not open — [`MemberList::unlisted`] is the same gap, and it is why this is `Unknown`.
        return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(format!(
            "{class}::{}",
            access.member
        ))));
    }

    Known::Yes(ProjectDefinitions {
        found,
        // A member behind a conditional `#if` inside a class body is a fact of its file like any other, and
        // `members_of` offers what it found; the count is about *includes*, which is a question this path never
        // asks. Zero rather than a guess.
        conditional: 0,
    })
}

/// **[`definition_across_files`] as a list** — see [`ProjectIndex::definitions`] for what the list is.
///
/// The two answers are one query: this one, and the single-answer form restricted to the names that have exactly
/// one declaration. A consumer that can show several locations should ask this one, because "ambiguous" is not an
/// answer a client can use.
pub fn definitions_across_files(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinitions> {
    // **A cursor on a member asks about the object's type, not about the name.** `line.empty()` is
    // `std::basic_string::empty`, and asking by name instead reaches every `empty` in the library — measured, 12
    // declarations against `basic_string`'s 2. So the member question goes first, and the name query answers only
    // when the member question *cannot be asked*:
    //
    // * the object's type is unknown — a heuristic answer beats a dead end, and this is what answered before the
    //   member path existed;
    // * a type that **is** known and does not have the member is not that case: answering with another class's
    //   member of the same name would be a jump to a place the language does not name, which the handler's own
    //   rule ("a wrong location is worse than none") forbids.
    if crate::sema::resolve::member_access_at(root, offset).is_some() {
        match member_definitions_across_files(index, scopes, root, path, offset) {
            Known::Yes(found) => return Known::Yes(found),
            // The member question could not be asked (the object's type is unknown) **or** was answered with
            // nothing, and both fall through to the name query — which is what answered before the member path
            // existed. The second case is a deliberate trade rather than an oversight: a member list is never a
            // claim of completeness ([`MemberList::unlisted`]), so "this class does not have that member" and "this
            // class's bases could not be read" arrive here looking the same, and a name-based list is a jump that
            // usually lands right rather than no jump at all. Measured: `std::cin.eof()` is inherited through
            // `basic_ios`, and while `std::basic_istream` is declared three times the base walk cannot open it —
            // the name query still has `eof` among its candidates.
            Known::Unknown(_) | Known::No => {}
        }
    }

    match crate::sema::resolve::definition_at(scopes, root, offset) {
        Known::Yes(binding) => {
            // The scope the answer was reached *through*, when the cursor wrote one: `ns` for `ns::Widget`. A bare
            // name has none, and a leading `::` means the global name space, which is no scope at all.
            let scope = crate::sema::resolve::qualified_name_at(root, offset).and_then(|(written, _)| {
                written
                    .rsplit_once("::")
                    .map(|(scope, _)| scope.trim_start_matches("::").to_string())
                    .filter(|scope| !scope.is_empty())
            });

            // A definition answer can be a *local*: the cursor is inside the same body the name was declared in,
            // which is exactly when `definition_at` resolves it here rather than sending it to the index. Saying
            // so matters because the caller may hand this answer on to a consumer asking whether the name is
            // reachable elsewhere. Asked before the binding is moved, and from the tree that can answer it.
            let local = scopes.declares_a_local(binding.scope);

            return Known::Yes(ProjectDefinitions::one(ProjectDefinition::from_binding(
                path, binding, scope, local,
            )));
        }
        Known::Unknown(UnknownReason::NotDeclaredHere(name)) => {
            // The single-file layer has already established the spelling, so the project layer is asked about
            // exactly that name rather than re-reading the cursor — and it answers with the list, which is the
            // whole point of this function.
            return index.definitions(&name, path);
        }
        Known::Unknown(reason) => return Known::Unknown(reason),
        Known::No => {}
    }

    // `No` from the single-file layer means the offset is not in a name the scopes could place at all — on
    // punctuation, or past the end — so there is nothing to look up anywhere.
    Known::Unknown(UnknownReason::UnparsableName)
}

/// The answer to a cross-file question about a macro name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMacro {
    pub file: PathBuf,
    /// The `#define` or the `#undef` that settles what the name is at the point asked about.
    pub fact: MacroFact,
}

/// Where the macro name written at `offset` is defined — or that it is not a macro there.
///
/// The entry point for "go to definition" on a macro, and the counterpart of [`definition_across_files`]. It needs
/// no scope tree and no second layer, because a macro query is not a name lookup at all: it is a question about
/// **translation order** — which of the `#define`s and `#undef`s written before this point is the last one — and
/// the index already stores every one of them with the offset it was written at.
///
/// [`UnknownReason::UnparsableName`] when the offset is not on a name at all, which is the ordinary answer for
/// most cursor positions.
pub fn macro_across_files(
    index: &ProjectIndex,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectMacro> {
    match crate::sema::resolve::name_at(root, offset) {
        Some((name, _)) => index.macro_definition(&name, path, offset),
        None => Known::Unknown(UnknownReason::UnparsableName),
    }
}

/// **One file's cooked reading, as the index keeps it** — what a compiler sees written there, and what the parse of
/// that text complained about.
///
/// The two travel together because they are one reading of one rendering: a caller that handed over the declarations
/// and dropped the errors would make the index answer "nothing is wrong with this file" for a rendering the parser
/// had something to say about, and a caller that replaced the declarations without replacing the errors would
/// publish the *old* file's errors beside the new file's names.
#[derive(Debug, Clone, Default)]
pub struct CookedFile {
    /// The declarations the rendering read as, every range mapped back into the file.
    pub declarations: Vec<DeclFact>,
    /// The rendering's parse errors, placed in the file — see [`crate::CookedDiagnostic`].
    pub diagnostics: Vec<crate::CookedDiagnostic>,
    /// Errors the parse of the rendering reported that **this file cannot show**, because the text they are about is
    /// not written here.
    ///
    /// Kept with the reading rather than with the caller, because it is what makes "no errors" honest: an empty
    /// [`CookedFile::diagnostics`] beside a non-zero count here means the rendering was not clean and the reader
    /// simply cannot be shown where.
    pub unplaced: usize,
}

impl CookedFile {
    /// A reading that found declarations and nothing to report.
    ///
    /// For a caller with facts and no parse of its own — a test that built a rendering by hand, or a future
    /// producer that reads the declarations back from a cache.
    pub fn declarations(declarations: Vec<DeclFact>) -> Self {
        CookedFile {
            declarations,
            diagnostics: Vec::new(),
            unplaced: 0,
        }
    }
}

impl From<crate::IndexedRendering> for CookedFile {
    /// What the index keeps of a rendering's reading: the declarations, the errors this file can show, and the count
    /// of the ones it cannot — and **not** the summary they came out of, because a rendering's summary describes text
    /// this crate spelled out: its directives and includes are empty, and keeping it would invite a reader to believe
    /// them (see the `cooked` field).
    fn from(indexed: crate::IndexedRendering) -> Self {
        CookedFile {
            declarations: indexed.summary.declarations,
            diagnostics: indexed.diagnostics,
            unplaced: indexed.unplaced,
        }
    }
}

/// A project's summaries, and the queries that need more than one of them.
///
/// Built incrementally: [`ProjectIndex::insert`] takes a summary that some other layer produced, which is what
/// keeps this type free of any opinion about parsing, caching or the filesystem.
#[derive(Debug, Default)]
pub struct ProjectIndex {
    /// The summaries, by normalized path.
    summaries: HashMap<String, FileSummary>,
    /// The paths **by sequence number**, which is insertion order: a query over all of them is deterministic rather
    /// than `HashMap`-ordered. A definition jump that returned a different file on each run would be a bug that only
    /// shows up in a test that runs twice.
    ///
    /// A map and not a `Vec` so that forgetting a file is a removal rather than a scan of every path — an edit
    /// forgets its file, and on a project of a hundred thousand files a `retain` per keystroke is a cost with
    /// nothing in it that answers anything.
    order: BTreeMap<u32, String>,
    /// The sequence number each file was given the first time it appeared: the reverse of `order`, and the identity
    /// the name index's postings use. **Never reused** — a forgotten file's number stays retired, so a posting can
    /// never come to point at a different file than it was made for.
    sequence: HashMap<String, u32>,
    next_sequence: u32,
    /// The inverted index — see [`crate::index::names`]. Derived from `summaries` and `cooked`, and updated in the
    /// same call that changes either; nothing else writes it.
    names: NameIndex,
    /// For each file, the files that include it. Derived from the summaries; see the module documentation.
    included_by: HashMap<String, BTreeSet<String>>,
    /// The macros a **compilation** starts with, which no summary can hold: the compiler's predefined names and the
    /// command line's `-D`s.
    ///
    /// Every condition stored in a summary is a question about these, and an index that has not been told them
    /// answers `Unknown` for every condition — which is what this type did before the field existed, and what it
    /// still does for a caller that builds an index by hand. See [`crate::index::environment`] for what the field
    /// is complete about and what it deliberately is not.
    macros: Marked,
    /// **What the cooked reading found**, by normalized path — the declarations a compiler sees, and the errors the
    /// parse of the rendering reported.
    ///
    /// A second reading beside the first, and only these two things of it, for reasons that are all about not
    /// lying: a rendering has no directives, so a cooked summary says `includes: []`, `macros: []`, `guards: []` — a
    /// reader that took those at face value would conclude the file includes nothing and defines nothing — and the
    /// two things a rendering knows that the file's own text does not are what it **declares** (the type a macro
    /// declared, `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND`; the *scope* a namespace-opening macro put it
    /// in, MSVC's `_STD_BEGIN` being `namespace std {`) and what the **parser said about it** — an error against
    /// text no branch, macro or conditional, keeps out of the compiler's sight.
    ///
    /// **Sparse on purpose**: cooking needs the translation unit's environment, so a caller has this for the files
    /// it actually read rather than for the whole project, and a file with no entry here is one nobody cooked.
    cooked: HashMap<String, CookedFile>,
    /// What the **condition** on a guarded `#include` was last answered, by `(file, region)`.
    ///
    /// A memo, not a fact: it is cleared whenever a summary is inserted, because that is the only thing that can
    /// change an answer. It exists because the answer is expensive — evaluating one condition builds the file's
    /// whole closure state (`macros_at`) — and the same question is asked once per query and once per walk, over
    /// hundreds of edges. `std::sync::Mutex` rather than a `RefCell` so the index stays `Sync`: a language server
    /// holds one of these behind a lock, and a cache that cost that property would be a bad trade.
    visibility_answers: std::sync::Mutex<HashMap<(String, u32), crate::Visibility>>,
}

    /// Which member a `obj.member` or `ptr->member` at `offset` names.
///
/// The first query in this crate that needs a **type**: `size` in `widget.size` is not looked up among the names
/// in scope, it is looked up *in the type of `widget`*. So the answer is built in three steps, each of which
/// already existed:
///
/// ```text
/// 1. read the shape          — the object expression and the member's spelling (sema::resolve)
/// 2. infer the object's type — its declaration's `type_of`, in this file or through the index
/// 3. look the member up      — as the qualified name `<type>::<member>`, which the index already matches
/// ```
///
/// Step 3 is why this was cheap to add: a declaration fact records the qualified spelling of the scope it was
/// written in, so `Widget::size` is a question the existing lookup answers. What is new is step 2, and it is the
/// beginning of the `infer` layer — deliberately narrow: the object has to be a
/// **name**, because inferring the type of an arbitrary expression is a different and much larger problem.
///
/// # The four answers
///
/// * `Yes` — the member, declared in the class the object's type names.
/// * `Unknown(UnknownType)` — the object's type could not be worked out: the object is not a plain name, or its
///   declaration says nothing about its type, or the type's name resolves to nothing.
/// * `Unknown(NotDeclaredHere)` — the type is known and the member is not in it. Not a definite no, for the
///   reason every other answer here is not: the class may be a base class, a template, or declared in a header
///   nobody indexed.
/// * `Unknown(UnparsableName)` — the offset is not on a member access at all.
pub fn member_across_files(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinition> {
    let Some(access) = crate::sema::resolve::member_access_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    // `w.` with nothing after the operator is a member access with no member, and a *jump* needs something to jump
    // to. The question this answers is which declaration the written name refers to, and there is no written name —
    // so this is the same "nothing to look up" as a cursor on punctuation. That the shape is readable at all is
    // what the completion query needs, and it is why the tolerance lives in the shape reader rather than here.
    if access.member.is_empty() {
        return Known::Unknown(UnknownReason::UnparsableName);
    }

    // The type of the object, which is what decides which class the member is looked for in.
    let Known::Yes((written, _)) = type_of_expression(index, scopes, root, path, &access.object, 0)
    else {
        let Known::Unknown(reason) =
            type_of_expression(index, scopes, root, path, &access.object, 0)
        else {
            unreachable!("the first match established that this is an `Unknown`")
        };
        return Known::Unknown(reason);
    };

    let class = base_type_name(&written);
    if class.is_empty() {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    let found = member_fact(index, scopes, root, path, class, &access.member);
    let Known::Yes((fact, file)) = found else {
        let Known::Unknown(reason) = found else {
            unreachable!("the first match established that this is an `Unknown`")
        };
        return Known::Unknown(reason);
    };

    Known::Yes(ProjectDefinition { file, fact })
}

/// Every member a type has: its own, and the ones it inherits.
///
/// The query a completion after `.` or `->` is built on, and the second one in this module that needs a **type**.
/// It is the *list* form of [`member_across_files`]: that one is given a member's name and answers where it is
/// declared, this one is given nothing but the type and answers what there is to name at all.
///
/// # The base chain is walked here, and never stored
///
/// The one design decision this query makes, and the reason it is a query rather than a field.
///
/// A summary keeps what each file **says**: `struct D : public B` is stored as the spelling `B`, and `D`'s
/// summary says nothing about `B`'s members. The alternative — resolving the bases when the summary is built and
/// writing `D`'s inherited members onto `D` — is what an index-shaped instinct reaches for, and it is wrong for
/// three reasons that all end in a stale answer with nothing on disk to contradict it:
///
/// ```text
/// 1. B gains and loses members          -> D's text and D's key are unchanged, and D's stored list is now wrong
/// 2. D's base list changes              -> caught, because D's text changed
/// 3. *which* B the name `B` means       -> unchanged in D's text, changed by a macro, an include or an
///                                          `#undef`, so D's stored list is wrong with nothing to catch it
/// ```
///
/// The third is the one that settles it: it leaves `D`'s text identical, so no per-file invalidation can see it.
/// Walking the chain at query time costs one lookup per base per query and cannot go stale, because nothing is
/// kept. `a_member_added_to_a_base_appears_without_reindexing_the_derived_class` is that argument as a test.
///
/// # The three things a list can be, and none of them is "these are all the members"
///
/// * `Yes(list)` — the members this analysis can see. `list.unlisted` names the bases that could not be listed
///   at all, so a consumer can tell a complete answer from a truncated one instead of guessing.
/// * `Unknown(NotDeclaredHere)` — nothing visible declares the type. Not `No`: the index holds a subset of the
///   translation unit, so a type from a header nobody indexed looks exactly like a type that does not exist.
/// * `Unknown(ConditionalCompilation)` — the type is only reachable through a guarded `#include`, so whether it
///   is here at all is not known.
///
/// # What it does with a name two bases declare
///
/// Both entries are listed and both are marked [`ProjectMember::ambiguous`]. Dropping one would be choosing, and
/// choosing is the answer the language refuses to give — the same fact [`member_across_files`] states as
/// `Unknown(Ambiguous)` for a single name, stated here as a property of a listed member, because a list with a
/// hole in it would be a worse answer than a list that says which entries are contested.
///
/// # What it deliberately does not do
///
/// * No `using` declarations and no virtual/override resolution. A `using Base::f;` in a derived class brings a
///   name in without declaring a member of its own, and nothing here models that yet.
/// * No access check. A `private` base's members are listed, because access is not in the facts — see
///   [`DeclFact::bases`] — and filtering on a guess would hide members a consumer can legitimately see.
/// * No instantiation. A template class lists the members it was written with; a base written `Base<int>` is
///   looked up as the class `Base`.
/// * No conditional region. A member of a class in the buffer comes back with [`FactGuard::Unconditional`]
///   whatever `#if` it is really in — see `fact_from_binding`. A class from the index does carry its regions.
pub fn members_of(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<MemberList> {
    // The spelling is normalized once, at the entry, for the same reason a base's is: a consumer feeding this from
    // `DeclFact.type_of` hands over what the file wrote — `const Widget&`, `::Widget`, `Base<int>` — and every one
    // of those parts is about the type's shape rather than about which class declares the members. Normalizing
    // here also means `declared_in` is a qualified spelling from the first level on, and levels cannot disagree.
    //
    // **And aliases are followed here as well as one level down**, which the measurement is why for: `direct_members`
    // and `direct_member` resolve the spelling before looking anything up, but the *base walk* below asks `bases_of`
    // about the class as it was written — and an alias has no bases, so `members_of("istream")` walked no bases,
    // recorded no gap, and answered 42 members with `unlisted` **empty**, while the resolved spelling
    // (`std::basic_istream`) answered the same 42 and *named* the gap. Two spellings of one type, one of them
    // claiming a completeness it does not have.
    let class = resolve_aliases(index, scopes, root, path, base_type_name(class));
    let class = &*class;

    let own = match direct_members(index, scopes, root, path, class) {
        Known::Yes(members) => members,
        Known::Unknown(reason) => return Known::Unknown(reason),
        // `direct_members` reports a name nothing declares as `Unknown(NotDeclaredHere)` rather than as `No` —
        // the index is a subset of the translation unit — so this arm exists for totality, not for a case.
        Known::No => return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    };

    let mut list = MemberList::default();
    let mut hidden: HashSet<String> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::from([class.to_string()]);

    // Level 0 is the type's own body. It is not part of the walk below because its members are the ones that
    // *hide*, and a base is only reached after them.
    let mut own: Vec<ProjectMember> = own
        .into_iter()
        .map(|(file, fact)| ProjectMember {
            file,
            fact,
            declared_in: class.to_string(),
            depth: 0,
            ambiguous: false,
        })
        .collect();
    own.sort_by(|one, other| one.fact.name.cmp(&other.fact.name));
    mark_ambiguity(&mut own);
    hidden.extend(recorded_names(&own));
    list.members.extend(own);

    // Then outward, one level of bases at a time — which is the order C++ hides in: a base's member is hidden by
    // a same-named member of anything nearer, so a level that has already contributed a name removes it from
    // every level below.
    //
    // Each base is carried with **the class whose base-clause wrote it**, because that is what decides which scope
    // an unqualified base name is looked up in: see `resolved_in_the_enclosing_scopes`.
    //
    // The class's **own** base list is the one case the walk below cannot report, and it is reported here instead,
    // exactly as a base's is: a class whose bases cannot be read is a truncated list, not a class with no bases.
    // Measured, and it is why the two look different from outside: MSVC's `<istream>` declares `std::basic_istream`
    // **three** times (the class, then `template class _CRTIMP2_PURE_IMPORT basic_istream<char, …>;` twice under
    // `#if defined(_DLL_CPPLIB)`), an ambiguous name answers no base list, and the members inherited from
    // `basic_ios` — `eof`, `fail`, `clear`, … — vanished from the list while `unlisted` stayed **empty**, so the
    // answer did not say it was incomplete.
    let mut level: Vec<(String, String)> = match bases_of(index, scopes, root, path, class) {
        Known::Yes(bases) => bases
            .into_iter()
            .map(|base| (base, class.to_string()))
            .collect(),
        Known::Unknown(reason) => {
            list.unlisted.push(UnlistedBase {
                spelling: class.to_string(),
                reason,
            });
            Vec::new()
        }
        Known::No => Vec::new(),
    };
    let mut depth = 1;

    while !level.is_empty() {
        let mut found: Vec<ProjectMember> = Vec::new();
        let mut next: Vec<(String, String)> = Vec::new();

        for (written, owner) in level {
            let base = resolved_in_the_enclosing_scopes(index, scopes, path, &owner, &written);
            if !visited.insert(base.clone()) {
                continue;
            }

            let members = match direct_members(index, scopes, root, path, &base) {
                Known::Yes(members) => members,
                // A base nothing here can resolve. Its members are **missing from the list** rather than absent
                // from the type, and naming the base is what makes the gap actionable — the fix is an include
                // path or a file that was never indexed, not a different query.
                Known::Unknown(reason) => {
                    list.unlisted.push(UnlistedBase {
                        spelling: base,
                        reason,
                    });
                    continue;
                }
                Known::No => continue,
            };

            found.extend(members.into_iter().map(|(file, fact)| ProjectMember {
                file,
                fact,
                declared_in: base.clone(),
                depth,
                ambiguous: false,
            }));

            match bases_of(index, scopes, root, path, &base) {
                Known::Yes(further) => {
                    next.extend(further.into_iter().map(|base_of_base| (base_of_base, base.clone())))
                }
                Known::Unknown(reason) => list.unlisted.push(UnlistedBase {
                    spelling: base,
                    reason,
                }),
                Known::No => {}
            }
        }

        found.retain(|member| !hidden.contains(&member.fact.name));
        found.sort_by(|one, other| one.fact.name.cmp(&other.fact.name));
        mark_ambiguity(&mut found);
        hidden.extend(recorded_names(&found));
        list.members.extend(found);

        level = next;
        depth += 1;
    }

    Known::Yes(list)
}

/// Every declaration written **directly in** the class or namespace `class` names.
///
/// The class's own body first, from the file being edited, and then the index — the same two-layer split
/// [`direct_member`] makes, and for the same reason: a buffer that has never been saved has no summary, and a
/// class the buffer does not mention is only in the index.
///
/// [`Known::Yes`] with an empty list is a real answer — a class with nothing in it — and is deliberately not the
/// same as [`Known::Unknown`], which is what a name nothing declares produces. The two are told apart by asking
/// whether the *name* is declared, which is the one question that distinguishes "nothing written in it" from
/// "nothing here knows what it is".
///
/// A name that is not an identifier — a destructor, an operator, a conversion function — comes back with an empty
/// [`DeclFact::name`], exactly as the index stores it. It is a declaration that exists, so it is not dropped here;
/// a consumer that shows a list filters on the name it can print and reads the spelling from the source, which is
/// where it lives.
fn direct_members(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<(PathBuf, DeclFact)>> {
    // Followed here as well as in `direct_member`, for the same reason it is there: the members of `std::string`
    // are the members of `std::basic_string`, and the member-list query and the single-member query must not
    // disagree about which class a spelling names.
    let class = &resolve_aliases(index, scopes, root, path, class);

    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(data) = scopes.scope(scope)
    {
        return Known::Yes(
            data.bindings
                .iter()
                .map(|binding| (path.to_path_buf(), fact_from_binding(root, class, binding)))
                .collect(),
        );
    }

    let found = index.declarations_in(class, path);
    if !found.is_empty() {
        return Known::Yes(
            found
                .into_iter()
                .map(|declaration| (declaration.file.clone(), declaration.fact.clone()))
                .collect(),
        );
    }

    // Nothing is written *in* it, so the name is either an empty class or no class at all. Which one is decided by
    // the declaration itself rather than by the absence of members: a fact whose own qualified name is the
    // spelling asked about is the class, and everything else that matched did so on its bare name.
    match index.definition(class, path) {
        Known::Yes(found) if found.fact.qualified_name() == *class => Known::Yes(Vec::new()),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::Yes(_) | Known::No => {
            Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class.as_str())))
        }
    }
}

/// The names in a level that take part in **hiding**: every member except the ones whose name is not recorded.
///
/// A destructor, an operator and a conversion function come back with an empty [`DeclFact::name`], because a fact
/// stores a lookup key rather than a spelling — the spelling lives in the source, and `~D` has no identifier in it.
/// There is therefore nothing to compare them by, and two of them are *not* one name: `~D` and `~B` differ by the
/// class they name. Leaving them out of the hiding rule is what keeps `~D` from hiding `~B` — a wrong answer
/// arrived at by treating a missing spelling as if it were a spelling. They are still **listed**, because they are
/// declarations that exist; see [`direct_members`].
fn recorded_names(members: &[ProjectMember]) -> impl Iterator<Item = String> + '_ {
    members
        .iter()
        .map(|member| member.fact.name.clone())
        .filter(|name| !name.is_empty())
}

/// Flag the members whose name another class at the same level also declares.
///
/// Level-scoped rather than list-scoped, and the difference is the language's: a base's member that a *derived*
/// class redeclares is hidden, so it never reaches this function, while two bases at the same level genuinely
/// leave the name unresolved — the same finding [`member_across_files`] reports as `Unknown(Ambiguous)`.
///
/// **Overloads are not ambiguity.** `void f(); void f(int);` declares one name once, in one class, and a consumer
/// that flagged it would refuse to complete a name the language resolves perfectly well. So what is counted is
/// the number of distinct declaring classes, not the number of declarations.
///
/// A member with no recorded name is skipped for the same reason it takes no part in hiding — see
/// [`recorded_names`] — and skipping it is the conservative direction: claiming two declarations are one contested
/// name would be a definite statement about a name this layer cannot read.
fn mark_ambiguity(members: &mut [ProjectMember]) {
    let mut declaring: HashMap<String, HashSet<String>> = HashMap::new();

    for member in members.iter().filter(|member| !member.fact.name.is_empty()) {
        declaring
            .entry(member.fact.name.clone())
            .or_default()
            .insert(member.declared_in.clone());
    }

    for member in members.iter_mut() {
        member.ambiguous = !member.fact.name.is_empty()
            && declaring
                .get(&member.fact.name)
                .is_some_and(|classes| classes.len() > 1);
    }
}

/// What a completion at a member access should offer: the type, its members, and where to put the chosen one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCompletions {
    /// The type of the object, as the **lookup key** the members were found under — `Widget`, `ns::Widget`,
    /// `Base` for `Base<int>`.
    ///
    /// The key rather than the spelling the file wrote, because a consumer showing "members of `…`" has to name
    /// something the user can find, and `Base<int>` is a type no member is declared in. It is also the fastest
    /// thing to look at when the list is wrong.
    pub class: String,
    pub members: MemberList,
    /// The range the member's name occupies, which is what a client **replaces** with the chosen one.
    ///
    /// Empty and just past the operator for `w.`, which is the state this query exists for. One field for both
    /// states on purpose: an insert-at-a-point and a replace-a-prefix are the same edit, and a consumer that had
    /// to branch on which one it is would get it wrong exactly once — on the keystroke right after the dot.
    pub member_range: cpp_parser::SourceRange,
    /// What of the name is already written **before the cursor**: `si` for `w.si|`, and empty for `w.|`.
    ///
    /// Not the whole written member, which is why it is computed here rather than taken from the shape: a cursor
    /// in the *middle* of a name — `w.si|ze` — filters by `si` while [`MemberCompletions::member_range`] covers
    /// all of `size`, because the range is what gets replaced and the prefix is what gets matched. A consumer that
    /// used the whole spelling for both would offer nothing while the user is plainly typing.
    pub prefix: String,
}

/// The members a completion at `offset` should offer: `w.` and `w.si`, answered with `Widget`'s members.
///
/// The third query that needs a **type**, and the one the other two were building towards: [`members_of`] answers
/// "what does this type have" given a type, and this answers "what should be offered here" given a cursor. The
/// steps are all ones that already existed, which is the whole reason it is short:
///
/// ```text
/// 1. read the shape   — the object expression and what of the member is written (completion::context_at)
/// 2. infer the type   — type_of_expression, the recursive inference the member *lookup* already uses
/// 3. list the members — members_of, including the ones the bases declare
/// ```
///
/// # The state it exists for
///
/// `w.` — the operator typed and nothing after it. That is the keystroke that *asks* the question, and the reader
/// above is the one place that decides it is a member access: the parser usually reads `w.` as an access with an
/// empty member, and where a recovery loses that shape the reader falls back to the **text**, which cannot be
/// ambiguous about an operator sitting at the cursor. The same answer covers `w.si`, where the work is only that
/// [`MemberCompletions::member_range`] is the written prefix instead of a point.
///
/// # The answers, and the one that matters most
///
/// * `Yes(completions)` — the members of the object's type.
/// * `Unknown(UnknownType)` — the object's type could not be worked out. **The common case in real code**, and
///   the honest one: `f().size` needs a return type computed, `(*p).size` needs a dereference followed, and
///   `arr[i].size` needs a subscript — see [`members_of`]'s note. A consumer shows
///   nothing rather than offering the members of some other type.
/// * `Unknown(NotDeclaredHere)` — the object's type is a class nothing visible declares.
/// * `Unknown(ConditionalCompilation)` — that class is only reachable through a guarded `#include`.
/// * `Unknown(UnparsableName)` — the cursor is not on a member access at all.
pub fn member_completions_at(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<MemberCompletions> {
    // **The shape comes from the completion context, which is the one reader of it.** This used to call
    // `sema::resolve::member_access_at`, which reads the access out of the tree alone, and the two disagree about
    // one offset a client really sends: with the caret drawn **on** the operator's own column (`full2|.`), the
    // grammar's access node ends before the operator, so the tree answers "not a member access" while the context
    // reader — which asks the text as well, for exactly this reason — answers "a member access with nothing
    // written". Measured on a live server: the member list and the diagnostic for the same keystroke said opposite
    // things, and a client asking at the caret's own column got the names in scope.
    //
    // Two readers of one fact is the shape of every bug in this area; the completion layer owns this question, and
    // every consumer of it — this query, `Session::member_completions`, the log line — goes through it.
    let crate::completion::CompletionContext::Member(access) = crate::completion::context_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let written = match type_of_expression(index, scopes, root, path, &access.object, 0) {
        Known::Yes((written, _)) => written,
        Known::Unknown(reason) => return Known::Unknown(reason),
        // The only `No` the type layer produces is "nothing says what this is", which is the same answer as an
        // expression it cannot type: there is nothing to list members of.
        Known::No => {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(
                access.object.text().to_string().trim(),
            )));
        }
    };

    let class = base_type_name(&written);
    if class.is_empty() {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    match members_of(index, scopes, root, path, class) {
        Known::Yes(mut members) => {
            // **The names the implementation owns are not offered**, the same rule the name completion applies —
            // and this is where a user meets them first: `line.` on a `std::string` listed 202 members of MSVC's
            // `basic_string`, half of them `_Alty`, `_ALLOC_MASK`, `_Apply_annotation`. See
            // [`is_reserved_to_the_implementation`].
            members.members.retain(|member| !is_reserved_to_the_implementation(&member.fact));

            Known::Yes(MemberCompletions {
                class: class.to_string(),
                members,
                member_range: access.member_range,
                prefix: written_before_the_cursor(&access, offset),
            })
        }
        Known::Unknown(reason) => Known::Unknown(reason),
        // `members_of` reports a name nothing declares as `Unknown(NotDeclaredHere)` rather than `No`, so this arm
        // is for totality and says the same thing it would.
        Known::No => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    }
}

/// The part of the member's spelling that lies **before** the cursor.
///
/// Separate from the range because the two answer different halves of the same edit: the range is what the client
/// **replaces** with the chosen name — all of `size`, so nothing of the old spelling is left behind — while this
/// is what a server **matches** against the candidates, and a cursor inside a name has only typed the part before
/// it. Taking the whole spelling for both is what makes `w.si|ze` offer nothing.
///
/// The slice is by byte offset into the member's own text, so it is exactly the written prefix; a cursor that is
/// not on a character boundary (possible in an identifier with non-ASCII letters) falls back to the whole
/// spelling, which is the safe direction — a filter that matches too much still shows something.
fn written_before_the_cursor(access: &crate::sema::resolve::MemberAccess, offset: usize) -> String {
    let start = access.member_range.start_offset;
    let end = access.member_range.end_offset().min(offset);

    if end <= start {
        return String::new();
    }

    access
        .member
        .get(..end - start)
        .unwrap_or(&access.member)
        .to_string()
}

/// One name a completion can offer: where it is declared, and how far out it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedName {
    pub file: PathBuf,
    pub fact: DeclFact,
    /// How many scopes out from the cursor the declaration was found: `0` is a name declared in the scope the
    /// cursor is in — a local, or the scope the `::` named — and larger numbers are enclosing scopes, which C++
    /// only considers once the inner ones have been searched. The order of [`NameCompletions::names`].
    ///
    /// Recorded rather than left to be recomputed, for the reason [`ProjectMember::depth`] is: a consumer showing
    /// locals above globals, or grouping by where a name came from, would otherwise have to reconstruct the walk it
    /// was just handed the result of.
    pub depth: usize,
    /// **How far from the cursor the declaration is in the program text** — the field a consumer ranks by.
    ///
    /// [`OfferedName::depth`] counts *scopes*, which orders a lookup but does not answer the question a suggestion
    /// list is judged by: a local and a name from `zmmintrin.h` can both be depth 2. This says which of them is
    /// nearer — the buffer's own scope, the buffer, a header beside it, a header it includes, or a header only
    /// that header includes — and it is the same order C++ would find them in from the other end.
    ///
    /// Recorded here rather than re-derived by a consumer because the derivation needs the include *graph*, and
    /// the walk that produced this list already crossed it: see [`NameProvenance`].
    pub from: NameProvenance,
}

/// Where a name a completion offers was found, relative to the file the cursor is in.
///
/// Ordered by nearness, so that the ranking can use the enumeration itself as a tier and two consumers cannot
/// disagree about which of two provenances is closer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NameProvenance {
    /// A scope of the file being edited: a local, a parameter, a member of the class the cursor's body is in, or
    /// the names written directly in a `::`-qualified scope.
    Scope,
    /// Written at file scope in the file being edited.
    ThisFile,
    /// A header the file being edited includes directly.
    DirectInclude,
    /// A file reached only through another file — the standard library's own headers, mostly.
    IndirectInclude,
}

/// What a completion **in a name** should offer: the scope that was listed, and its names.
///
/// The sibling of [`MemberCompletions`], and the other half of "what can be typed here": that one answers after a
/// `.` or `->`, where the answer is a type's members; this one answers in a name position, where the answer is
/// what is visible — after a `::` (one scope), or with no qualifier at all (the scopes the cursor is inside, from
/// the innermost outward).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameCompletions {
    /// The scope the names were listed from, as the **lookup key** rather than as the file spelled it: `ns`,
    /// `ns::Widget`, `::` for the global name space, or empty when the cursor wrote no qualifier — there the names
    /// come from the scopes the cursor is inside rather than from one scope.
    pub scope: String,
    /// The names, innermost first and **deduplicated by name**: a declaration in an inner scope hides the same
    /// name in an outer one, which is what C++ does and what keeps a local `count` from being offered beside the
    /// header's. Two declarations of one name in the *same* scope are both kept — that is an overload, not a
    /// choice.
    pub names: Vec<OfferedName>,
    /// What of the name is already written **before the cursor**: `Wid` for `ns::Wid|`, empty for `ns::|`.
    ///
    /// The counterpart of [`MemberCompletions::prefix`], and separate from the range for the same reason.
    pub prefix: String,
    /// The range a client **replaces** with the chosen name: the written segment, or an empty range at the cursor
    /// when the segment has only just begun.
    pub name_range: cpp_parser::SourceRange,
}

/// The names a completion at `offset` should offer: `ns::`, `Widget::`, `::`, and a bare name.
///
/// # The four positions, and one query for all of them
///
/// ```text
/// ns::            one scope: the names written directly in `ns`
/// Widget::        one scope, and its **bases** — a typedef a base declares is nameable through the derived class
/// ::Widget        the global name space only, which is what the leading `::` asks for
/// loc             no qualifier: every name visible from the cursor, innermost scope first
/// ```
///
/// The first three are one question — list the declarations written in a scope — and the fourth is the same
/// question asked of a *chain* of scopes. What differs is only where the scopes come from, which is why this is one
/// query rather than four.
///
/// # The two layers, and when they are merged rather than chosen between
///
/// The file's own scope tree is asked first, because a buffer that has never been saved has no summary — the split
/// every cross-file query in this crate makes. What happens next depends on what the scope *is*, and the difference
/// is the language's:
///
/// * a **class** is defined once, so its members come from one place: the buffer's scope if it has one, the index
///   otherwise. Merging would list the same member twice;
/// * a **namespace** can be reopened, and a file that writes `namespace ns { … }` and includes a header that does
///   the same has both sets of names. So those are merged, the buffer winning a name they both declare;
/// * the **global name space** is a namespace, and is the namespace every included header writes into — the same
///   merge, with the file scope of the buffer as the first contributor.
///
/// # What it will not offer
///
/// A **local** declaration of another file is never in the list. The index cannot place a local in the function it
/// belongs to, so a name lookup there could only guess — and the guess would be wrong in the common case rather
/// than the rare one: the standard library's headers alone declare thousands of locals called `__first` and `n`.
/// See [`DeclFact::local`]. Locals of the file being edited *are* offered, from its own scope tree, which is the
/// only thing that knows which body they are in.
///
/// # The answers
///
/// * `Yes(completions)` — the names, which may be an empty list for a scope that is declared and empty.
/// * `Unknown(UnparsableName)` — the cursor is not in a name or a qualifier: on punctuation, on a keyword, or past
///   the end of what could be one.
/// * `Unknown(NotDeclaredHere)` — the `::` named a scope nothing here declares, so there is nothing to list and no
///   way to say it is empty.
/// * `Unknown(ConditionalCompilation)` — the scope is declared, but only in a file reached through a guarded
///   `#include`, so whether these names are visible depends on macros this layer does not have.
pub fn name_completions_at(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<NameCompletions> {
    let Some(position) = crate::sema::resolve::name_position_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    // **What is typed decides how much is read**, and the difference is not small. The index half of this query
    // collects every declaration of every file the cursor can see — measured on a real file that includes
    // `<string>`, **1103** names at one cursor — and collecting them to then drop all but the ones beginning with
    // `w` is work with no answer in it. A client asks on every keystroke, so the prefix is passed down and applied
    // where the names are still borrowed: see [`ProjectIndex::visible_declarations`].
    //
    // An **empty prefix must not filter**: that is the state the whole feature exists for (`return |`, a blank
    // line), and a filter that treated "nothing written" as "nothing matches" would empty the list exactly when it
    // is most wanted.
    let names = if position.scope.is_empty() {
        match visible_names(index, scopes, root, path, offset, &position.written) {
            Known::Yes(names) => names,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnparsableName),
        }
    } else {
        match names_in_a_scope(index, scopes, root, path, &position.scope, &position.written) {
            Known::Yes(names) => names,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnparsableName),
        }
    };

    Known::Yes(NameCompletions {
        scope: position.scope,
        names,
        prefix: position.written,
        name_range: position.range,
    })
}

/// Every name visible from a cursor with no qualifier written: the scope chain, then the index.
///
/// The order is C++'s: the scope the cursor is in, then each enclosing scope outward, and only then the names other
/// files contribute. Each step's own answer is already a list — the tree's bindings, a class's members including
/// its bases, a namespace's declarations across the project — so this walks the chain and hands each depth to the
/// function that knows how to list it.
fn visible_names(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
    written: &str,
) -> Known<Vec<OfferedName>> {
    let Some(innermost) = scopes.scope_at(offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let chain = scopes.scope_chain(innermost);
    let mut names: Vec<OfferedName> = Vec::new();

    // The file the cursor is in, as the store spells paths, so that the declaration-order filter can tell this
    // file's offsets from another file's.
    let in_this_file = normalize(path);

    for (depth, scope) in chain.iter().copied().enumerate() {
        let Some(data) = scopes.scope(scope) else {
            continue;
        };

        // The file scope is the global name space, and it is the one scope whose names other files contribute to
        // as well — every included header is written there. It is listed below, once, rather than here.
        if data.kind == crate::ScopeKind::TranslationUnit {
            continue;
        }

        // A **class** in the chain — the cursor is inside a member function of it — brings its members, bases
        // included: `size` is nameable inside `Widget::g` without a qualifier, and so is anything a base declares.
        if matches!(data.kind, crate::ScopeKind::Class | crate::ScopeKind::Enum)
            && let Some(class) = scopes.qualification_prefix_of(scope)
        {
            let members = match names_of_a_class(index, scopes, root, path, &class) {
                Known::Yes(members) => members,
                Known::Unknown(reason) => return Known::Unknown(reason),
                Known::No => Vec::new(),
            };
            names.extend(offered(members, depth));
            continue;
        }

        // Anything else in the chain is a body, a block or a lambda: its own bindings, which the tree has — and a
        // **namespace**, whose bindings are neither: the kind is what decides, and it is also what decides whether
        // a name beginning with an underscore belongs to the user (a body) or to the global name space (a `_name`
        // at file scope is the implementation's, one inside a function is not).
        let body = matches!(
            data.kind,
            crate::ScopeKind::Block | crate::ScopeKind::Function | crate::ScopeKind::Lambda
        );
        names.extend(offered(
            // **In a body, only what is declared above the cursor is in scope.** A *scope* holds every binding
            // written in its braces, so a list built from the scope contains `int c = a + b;` at a cursor three
            // lines above it — a name that cannot be written there, because the language has not declared it yet.
            // It is the same rule a reader applies without thinking, and the one a user reported: "在我下面声明的
            // 变量不应该补全出来".
            //
            // Only bodies: a class's members and a namespace's names are visible in **declaration order
            // independent** fashion (`class C { void f() { x = 1; } int x; };` is legal), so the filter would be
            // wrong there. Nothing is lost by the narrower rule — a body is exactly where the *textual* order is
            // the language's rule.
            bindings_of(root, path, &data.bindings, None, body)
                .into_iter()
                .filter(|candidate| !body || declared_above(candidate, offset, &in_this_file))
                .collect(),
            depth,
        ));

        // …and for a **namespace**, the names other files write into it. A namespace is reopened by every file
        // that mentions it, so the buffer's list is never the whole answer.
        if let Some(prefix) = scopes.qualification_prefix_of(scope) {
            names.extend(offered(
                declarations_in(&prefix, index, path, written),
                depth,
            ));
        }
    }

    // The global name space: the buffer's own file scope, and every file it includes. One step past the chain, so
    // that the depth stays an ordinary count of how far the lookup walked — the file scope is a scope like any
    // other, it is just not one the chain walked through.
    names.extend(global_offers(index, scopes, root, path, chain.len(), written));

    sort_and_hide(&mut names);
    Known::Yes(names)
}

/// The names at **file scope**, ready to offer: the buffer's own and every included file's, deduplicated.
///
/// A helper rather than two lines at each call site because the deduplication is the part that is easy to forget
/// and impossible to notice: the buffer's file-scope names and the index's are the *same* declarations whenever
/// the file being edited has been saved, so a list that skipped this step offers every global name twice.
fn global_offers(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    depth: usize,
    written: &str,
) -> Vec<OfferedName> {
    let mut names = offered(global_names(index, scopes, root, path, written), depth);
    sort_and_hide(&mut names);
    names
}

/// The names written directly in one `::`-qualified scope.
fn names_in_a_scope(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    scope: &str,
    written: &str,
) -> Known<Vec<OfferedName>> {
    // The global name space has no spelling of its own: `::` is what the file writes, and `None` is what a
    // declaration at file scope records.
    if scope == "::" {
        return Known::Yes(global_offers(index, scopes, root, path, 0, written));
    }

    // A leading `::` on a longer path — `::ns::Widget` — asks for the *global* one, and the spelling to look up is
    // the rest: a fact records `ns::Widget` and never `::ns::Widget`, for the same reason.
    let spelling = scope.trim_start_matches("::");

    let in_the_tree = scopes.scope_with_qualified_name(spelling);
    let kind = in_the_tree.and_then(|id| scopes.scope(id)).map(|scope| scope.kind);

    let mut names: Vec<OfferedName> = Vec::new();

    match kind {
        // A class is defined once, so its members come from one place — and the walk over its bases is
        // `members_of`'s job rather than this query's.
        Some(crate::ScopeKind::Class | crate::ScopeKind::Enum) => {
            let members = match names_of_a_class(index, scopes, root, path, spelling) {
                Known::Yes(members) => members,
                Known::Unknown(reason) => return Known::Unknown(reason),
                Known::No => Vec::new(),
            };
            names.extend(offered(members, 0));
        }
        // A namespace can be reopened, so the buffer's names and the project's are both part of the answer.
        Some(_) => {
            if let Some(data) = in_the_tree.and_then(|id| scopes.scope(id)) {
                // `false`: a namespace is not a body, so a `_name` in it is not a local — the reservation that
                // applies to a name beginning with an underscore is the *global* name space's, and the spelling
                // here is the namespace's own.
                names.extend(offered(
                    bindings_of(root, path, &data.bindings, Some(spelling), false),
                    0,
                ));
            }
            names.extend(offered(
                declarations_in(spelling, index, path, written),
                0,
            ));
        }
        // Not in the buffer at all: the project's answer, or nothing.
        None => {
            let found = declarations_in(spelling, index, path, written);
            if found.is_empty() {
                // Empty is either "declared and empty" or "no such scope", and the difference is the *name* — the
                // same distinction `direct_members` makes, for the same reason.
                return match index.definition(spelling, path) {
                    Known::Yes(found) if found.fact.qualified_name() == spelling => Known::Yes(Vec::new()),
                    Known::Unknown(reason) => Known::Unknown(reason),
                    _ => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(spelling))),
                };
            }
            names.extend(offered(found, 0));
        }
    }

    sort_and_hide(&mut names);
    Known::Yes(names)
}

/// **Is this declaration above the cursor**, so that the language has reached it?
///
/// The rule a reader applies without thinking, and the one a user reported as missing ("在我下面声明的变量不应该补全
/// 出来"): `int sum = a + b;` three lines *below* the cursor is not a name that can be written at the cursor, and a
/// list built from a scope's bindings contains it anyway — a scope holds every binding written inside its braces.
///
/// # Where it applies, and where it must not
///
/// Only in a **body** (a function, a block, a lambda), because that is the one place where textual order *is* the
/// language's rule. A class's members and a namespace's names are reachable regardless of where they are written —
/// `class C { void f() { x = 1; } int x; };` is perfectly legal — so filtering those by position would remove names
/// a reader can legitimately write.
///
/// # The one thing it will not judge
///
/// A binding from **another file**: its range is an offset into that file, so comparing it with this file's cursor
/// compares two different rulers. Those are kept — the conservative direction, and the same one the rest of this
/// module takes: a name that is in scope and not offered is a missing answer, while a name that is not in scope yet
/// and *is* offered is one the reader sees as a mistake.
///
/// In practice every binding here is the buffer's own — `build_scopes` walks one file's tree, and a name reached
/// through an include is a [`NameProvenance::DirectInclude`] fact from the index rather than a binding — so this is
/// the guard that keeps the rule from being wrong if that ever changes.
fn declared_above(candidate: &Candidate, offset: usize, in_this_file: &str) -> bool {
    if candidate.file != Path::new(in_this_file) {
        return true;
    }

    let at = candidate.fact.name_range.start_offset;

    // A zero-length range at the start of the file is what a recovery produces, and it is not a position.
    at == 0 || at < offset
}

/// One name on its way to being offered: where it was declared, how far inside one answer it was, and how near the
/// cursor that is.
///
/// Named rather than a tuple because there are four of them now and two of the four are `usize`; a call site that
/// had to remember which was which is how a ranking ends up ordered by the wrong number.
struct Candidate {
    file: PathBuf,
    fact: DeclFact,
    /// How far *inside* one answer the name was found — a base class's member is one step further than the class's
    /// own — which is added to the depth of the scope the answer came from.
    steps: usize,
    from: NameProvenance,
}

impl Candidate {
    fn new(file: PathBuf, fact: DeclFact, from: NameProvenance) -> Candidate {
        Candidate {
            file,
            fact,
            steps: 0,
            from,
        }
    }
}

/// How many declarations from **other files** one name query will collect.
///
/// A backstop on work rather than a statement about the answer, and the number is derived from what the answer can
/// be: `crate::completion` shows at most two hundred items, and every one of them is scored by *how near the cursor
/// its declaration is*. A query that has already collected this many names — all of them from files the cursor
/// reaches through includes, which is the tier that ranks last — cannot have its answer changed by the next one,
/// and on a file that includes `<string>` and the C++ standard library there are **26 728** declarations in the
/// files one cursor can see.
///
/// The file's **own** declarations are not capped: they come from the scope tree rather than from here, they are
/// the tier that ranks first, and there are as many of them as the reader wrote.
const MAX_COLLECTED_NAMES: usize = 400;

/// How many declarations one name may have before a workspace search stops ordering them — see
/// `ProjectIndex::open_group`.
const POPULAR_NAME: usize = 4096;

/// The bindings one of the file's own scopes holds, as names to offer. — `ns` for `namespace ns { … }`, a class's name for
/// its body — and `None` for a scope that has none: a function body, a block, a lambda. The distinction is the one
/// [`DeclFact::scope`] documents, and it is passed rather than derived because the caller is the one that knows
/// *why* it is listing these bindings.
///
/// `local` is the other half of that and it is **not** derivable from `scope`, which is why it is a parameter:
/// `None` is passed by two callers with different meanings — the **file scope** (whose names are the global name
/// space's) and a **body** (whose names are the user's locals) — and the difference decides what may be offered.
/// A `_name` at file scope is reserved to the implementation; the same spelling inside a function is the user's own
/// and is offered. Filling this in wrong is not visible anywhere else: the fact's `local` field is only read by
/// [`is_reserved_to_the_implementation`] on this path, and a local marked global silently disappears from the list.
fn bindings_of(
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    bindings: &[crate::Binding],
    scope: Option<&str>,
    local: bool,
) -> Vec<Candidate> {
    let spelling = scope.unwrap_or_default();

    bindings
        .iter()
        .map(|binding| {
            let mut fact = fact_from_binding(root, spelling, binding);
            fact.local = local;
            Candidate::new(path.to_path_buf(), fact, NameProvenance::Scope)
        })
        .collect()
}

/// The names written at **file scope**: this buffer's, and every included file's.
fn global_names(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    written: &str,
) -> Vec<Candidate> {
    let mut names: Vec<Candidate> = scopes
        .root()
        .and_then(|root_scope| scopes.scope(root_scope))
        // `false`: this **is** the global name space — the one scope where a name beginning with an underscore is
        // reserved to the implementation, which is why `Session::name_completions` does not offer `_Arg` or `_cprintf`.
        .map(|data| bindings_of(root, path, &data.bindings, None, false))
        .unwrap_or_default();

    // **The buffer's own file-scope names are the file's**, even though they live in the global name space: the
    // distinction the ranking needs is "did the reader write this here", not "which scope is it in".
    for name in &mut names {
        name.from = NameProvenance::ThisFile;
    }

    names.extend(declarations_in_scope(index, path, None, written));
    names
}

/// The facts the index holds for a scope spelling, or for the global name space when it is `None`.
///
/// The global case is the *absence* of a spelling rather than an empty one, which is why it cannot go through
/// [`ProjectIndex::declarations_in`]: that one compares a spelling, and a declaration at file scope records
/// `None`. It is the same distinction `matches` makes for a leading `::`. A **local** is left out here for the
/// reason [`DeclFact::local`] gives — a local also records `None`, and offering another file's would be the wrong
/// answer rather than a missing one.
fn declarations_in_scope(
    index: &ProjectIndex,
    visible_from: &Path,
    scope: Option<&str>,
    written: &str,
) -> Vec<Candidate> {
    // **The prefix is applied here, where the names are still borrowed.** Everything below this point is work per
    // declaration — a `DeclFact` clone, a provenance lookup, a `Vec` push — and on a file that includes `<string>`
    // there are over a thousand of them at one cursor position. Collecting them to drop all but those beginning
    // with `w` is the difference between a keystroke and a stutter, and it is a filter rather than a query: the
    // names it drops could not have been offered anyway. See [`crate::sema::resolve::name_position_at`].
    let accepts = |fact: &DeclFact| name_starts_with(fact, written);

    let found = match scope {
        Some(spelling) => index.declarations_in_where(spelling, visible_from, accepts),
        None => index.visible_declarations_where(visible_from, |fact| {
            accepts(fact) && fact.scope.is_none() && !fact.local
        }),
    };

    // The names a reader has **not** written are the last tier of the answer, and past this many of them there is
    // nothing left that could reach the top of a list two hundred long. See [`MAX_COLLECTED_NAMES`].
    let found = found.into_iter().take(MAX_COLLECTED_NAMES);

    found
        .map(|declaration| {
            Candidate::new(
                declaration.file.clone(),
                declaration.fact.clone(),
                provenance_of(index, visible_from, declaration.visibility, &declaration.file),
            )
        })
        .collect()
}

/// Does this declaration's name begin with what is being typed?
///
/// Case-insensitive, and **`true` for an empty prefix** — the state the whole feature is for. The rule is a
/// *filter* and not a ranking: the offer list drops a name this rejects, because a name the reader has not started
/// typing cannot be what they meant and the client would filter it out anyway. What it must not do is decide
/// between two names that both match; that is `crate::completion`'s job.
fn name_starts_with(fact: &DeclFact, written: &str) -> bool {
    if written.is_empty() {
        return true;
    }

    let name = fact.name.as_str();
    name.len() >= written.len()
        && name
            .chars()
            .zip(written.chars())
            .all(|(have, wanted)| have.eq_ignore_ascii_case(&wanted))
}

/// [`declarations_in_scope`] for a named scope, spelled the way this module spells lookups.
fn declarations_in(
    spelling: &str,
    index: &ProjectIndex,
    visible_from: &Path,
    written: &str,
) -> Vec<Candidate> {
    declarations_in_scope(index, visible_from, Some(spelling), written)
}

/// **How near the cursor a file is**, for the ranking: the buffer, a project header beside it, a header the buffer
/// includes, or one only those headers include.
///
/// The three cross-file answers are exactly the three the visibility walk produced — the file was reached by
/// crossing includes — so this only has to tell *which* of them a fact is, and the question it asks the graph is
/// the cheap one: does the file that declares this name appear among the cursor file's **own** `#include`s.
///
/// A file that is reachable only through a guarded `#include` ([`IncludeVisibility::Conditional`]) is ranked as an
/// indirect one, which is the honest place for it: it is in the list — the reader may be writing for exactly that
/// configuration — but it is not one of the headers this file certainly includes.
fn provenance_of(
    index: &ProjectIndex,
    visible_from: &Path,
    visibility: IncludeVisibility,
    declaring: &Path,
) -> NameProvenance {
    if visibility == IncludeVisibility::Conditional {
        return NameProvenance::IndirectInclude;
    }

    let declaring = normalize(declaring);
    let mut direct = false;

    if let Some(summary) = index.summary(visible_from) {
        for include in &summary.includes {
            let Some(resolved) = &include.resolved else {
                continue;
            };
            if normalize(resolved) == declaring {
                direct = true;
                break;
            }
        }
    }

    if direct {
        return NameProvenance::DirectInclude;
    }

    NameProvenance::IndirectInclude
}

/// The members of a class, bases included, as names to offer.
fn names_of_a_class(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<Candidate>> {
    match members_of(index, scopes, root, path, class) {
        Known::Yes(members) => Known::Yes(
            members
                .members
                .into_iter()
                .map(|member| {
                    // A class's own members are reachable from a cursor inside the class without a qualifier, and
                    // they are ranked as what they are — the nearest thing there is, beside a local. Which is why
                    // the provenance is `Scope` and the *depth* carries the inheritance: see `Score::LOCAL`.
                    let mut candidate = Candidate::new(member.file, member.fact, NameProvenance::Scope);
                    candidate.steps = member.depth;
                    candidate
                })
                .collect(),
        ),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    }
}

/// Wrap candidates as offers, adding the depth of the scope they were found in and dropping the declarations that
/// have no name to type — and the names the implementation owns.
///
/// [`Candidate::steps`] is how far *inside* one answer the name was — a base class's members are one step further
/// than the class's own — and the two are added because they mean the same thing to a consumer: how far the lookup
/// had to walk before it found this name, which is the order C++ walks in too.
fn offered(found: Vec<Candidate>, depth: usize) -> Vec<OfferedName> {
    found
        .into_iter()
        .filter(|candidate| !candidate.fact.name.is_empty())
        .filter(|candidate| !is_reserved_to_the_implementation(&candidate.fact))
        .map(|candidate| OfferedName {
            file: candidate.file,
            fact: candidate.fact,
            depth: depth.saturating_add(candidate.steps),
            from: candidate.from,
        })
        .collect()
}

/// Is this name **reserved to the implementation**, and therefore not something a completion should offer?
///
/// The standard's own three rules, and nothing else:
///
/// ```text
/// __name    reserved everywhere                `__crt_…`, `__imp_…`
/// _Name     reserved everywhere                `_ALLOC_MASK`, `_Alty`, `FILE`? no — `FILE` is not reserved
/// _name     reserved **in the global namespace**   `_Arg`, `_Address`, `_cprintf`
/// ```
///
/// The third is why this is asked of a **fact** rather than of a spelling: `_Address` at file scope is the
/// implementation's, while `Widget::_count` is the user's own member and a local `_i` is theirs too — and a
/// `DeclFact` says which it is (`local`, and `scope: None` for the global name space).
///
/// # What it is worth, measured
///
/// A completion is a suggestion list, and these are names the user may not use. On one real file, of the **1903**
/// names visible at a blank line inside a function body, **601** were reserved this way (`_Arg`, `_Arg1`,
/// `__crt_…`) — and the same rule takes `_ALLOC_MASK`, `_Alty` and `_Apply_annotation` off `basic_string`'s
/// member list, which is where a user completing `line.` meets them first.
///
/// # What it is *not*
///
/// Not a rule about what the analysis will answer: hover, a jump and a rename still find a reserved name — a reader
/// who points at `_Arg` in a header wants to know what it is — this only decides what to **offer**.
pub(crate) fn is_reserved_to_the_implementation(fact: &DeclFact) -> bool {
    let name = fact.name.as_str();
    let mut characters = name.chars();

    if characters.next() != Some('_') {
        return false;
    }

    match characters.next() {
        // `__name`, and a lone `_`.
        Some('_') | None => true,
        // `_Name`.
        Some(second) if second.is_uppercase() => true,
        // `_name`: the implementation's only in the global name space — and not for a local, whose scope is `None`
        // for the reason `DeclFact::scope` documents (a body contributes no segment to a qualified name).
        Some(_) => fact.scope.is_none() && !fact.local,
    }
}

/// Put the offers in the order a completion shows them: nearest scope first, then by name; and drop every name an
/// inner scope has already offered.
///
/// The hiding is what makes a *list* an answer rather than a pile: a local `count` and a header's `count` are two
/// declarations of one name, and C++ reaches only the first. Both would be wrong to show — one of them is not what
/// the user gets if they pick it — and the inner one is the one they get.
fn sort_and_hide(names: &mut Vec<OfferedName>) {
    // Stable, so that two declarations of one name at one depth keep the order they were found in: that is an
    // overload, and reordering it would be inventing a preference.
    names.sort_by(|one, other| {
        one.depth
            .cmp(&other.depth)
            .then_with(|| one.fact.name.cmp(&other.fact.name))
    });

    let mut seen: HashSet<String> = HashSet::new();
    names.retain(|name| seen.insert(name.fact.name.clone()));
}

/// How deep the type question may ask itself before it gives up.
///
/// See the guard in [`type_of_expression`]: this is a bound on an ill-formed file's recursion, not a statement
/// about how complex a type may be.
const MAX_TYPE_DEPTH: usize = 8;
/// The type of an expression, as far as this layer can tell, and the file that declared it.
///
/// The core of the `infer` layer, and it is **recursive** because that is what an expression is: `a.b.size` is a
/// member access whose object is a member access, and the type of the inner one is the type recorded on the
/// declaration the outer one starts from.
///
/// # The three shapes it can type, and the boundary
///
/// ```text
/// a name          `widget`      — its declaration's `type_of`, from this file or through the index
/// `this`          `this->size`  — the class whose scope encloses the expression, which needs no inference
/// a member access `a.b`         — recursively: find `b` in the type of `a`, then read `b`'s own `type_of`
/// ```
///
/// Everything else is [`UnknownReason::UnknownType`] carrying the expression's spelling: a call (`f().size`), a
/// dereference (`(*p).size`), a subscript, an arithmetic expression. Each of those needs a type *computed* rather
/// than read off a declaration, which is a different and much larger problem — and answering `Unknown` is what
/// keeps this layer from being wrong in a way a consumer cannot see.
pub(crate) fn type_of_expression(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Known<(String, PathBuf)> {
    let text = expression.text().to_string();
    let written = text.trim();

    // **A bound on the recursion, not a policy about types.** Every arm below can ask the same question about a
    // *smaller* expression, and one of them asks it about a *different declaration* — which is what makes a file
    // like `auto a = *b; auto b = *a;` a loop rather than a walk. Nobody writes that, and a language server meets
    // whatever is written: past this depth the answer carries the spelling and no type, which is the same answer
    // this layer gives everything it cannot read.
    if depth > MAX_TYPE_DEPTH {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    // `this` is the enclosing class, and no inference is involved: the scope chain already knows which class this
    // is, and it is the same answer inside every member function of it.
    if written == "this" {
        let offset = usize::from(expression.text_range().start());
        return match enclosing_class(scopes, offset) {
            Some(class) => Known::Yes((class, path.to_path_buf())),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A name: its declaration says what type it has. The file being edited is asked first, because a buffer that
    // has never been saved has no summary — the two-layer split the name query uses, for the same reason.
    //
    // **Qualified or bare is the same case**, and that is C++'s rule rather than a convenience: `lib::global` is one
    // entity, and the chain is part of its *spelling* — a scope to descend through, not an expression around a name.
    // Read the other way (a bare name only) every qualified name in every file was `UnknownType`: `lib::global`,
    // `std::cin`, `ns::make()` — measured, `a_qualified_name_in_this_file_has_the_type_its_declaration_wrote`.
    if writes_a_name(written) {
        return match declaration_of_expression(index, scopes, root, path, expression) {
            Known::Yes(named) => match declared_type(index, scopes, root, path, &named, depth) {
                Known::Yes(type_of) => Known::Yes((type_of, named.file(path))),
                Known::Unknown(reason) => Known::Unknown(reason),
                // The declaration is a function, a class or an alias: none of them has a type *as a name*, which
                // is the distinction `DeclFact::returns` exists for.
                Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
            },
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A **braced initializer**: `Widget{…}` is a temporary of the class it names, which is what makes
    // `auto w = Widget{}` a declaration of a `Widget`. Only the first child is read, and only when it *names* a
    // type: `{1, 2}` names nothing, and a braced list of values is a different thing that this layer does not
    // deduce (C++ deduces it as `std::initializer_list`, which is a library type, not a language one).
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::InitListExpr {
        let Some(first) = expression.children().next() else {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
        };

        return match type_of_expression(index, scopes, root, path, &first, depth + 1) {
            // The named type is a class, and a class has no `type_of` of its own — so the *spelling* is the answer,
            // which is what the initializer wrote.
            Known::Yes((type_of, file)) => Known::Yes((type_of, file)),
            _ => match declaration_of_expression(index, scopes, root, path, &first) {
                Known::Yes(named) if named.type_of(root).is_none() => {
                    Known::Yes((first.text().to_string().trim().to_string(), named.file(path)))
                }
                _ => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
            },
        };
    }

    // A **call**: what the callee returns, or a temporary of the class it names.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::CallExpr {
        return type_of_a_call(index, scopes, root, path, expression);
    }

    // A **parenthesised** expression has the type of what it wraps, and the parentheses are a node of their own:
    // `(*p).size` makes the object of the `.` a `ParenExpr`, so a recursion that did not step through it would
    // stop one level above the answer — which is exactly where it used to stop.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::ParenExpr {
        return match expression.children().next() {
            Some(inner) => type_of_expression(index, scopes, root, path, &inner, depth + 1),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A **dereference**: `*p` has the type `p` points at. Nothing is looked up — the pointer's own declaration
    // already spells the pointee, and the `*` is arithmetic on that spelling.
    if let Some(operand) = unary_operand_with(expression, "*") {
        let operand_type = type_of_expression(index, scopes, root, path, &operand, depth + 1);
        return match operand_type {
            Known::Yes((type_of, file)) => match pointee_type_name(&type_of) {
                Some(pointee) => Known::Yes((pointee, file)),
                // An operand whose type has no `*` on it: the program is ill-formed, and a type invented here
                // would be a wrong answer rather than a missing one.
                None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
            },
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // An **address-of**: `&x` is a pointer to `x`, which is the same arithmetic the other way round. It is here
    // so that the two operators are one rule rather than one rule and one hole: `(&r)->size` is a member access
    // whose object is this, and the `->` already knows what to do with a pointer.
    if let Some(operand) = unary_operand_with(expression, "&") {
        let operand_type = type_of_expression(index, scopes, root, path, &operand, depth + 1);
        return match operand_type {
            Known::Yes((type_of, file)) => {
                let pointed_at = pointee_type_name(&type_of).unwrap_or(type_of);
                Known::Yes((format!("{pointed_at}*"), file))
            }
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A **subscript**: `arr[0]` has the array's element type. That is the whole of it for an array; a *class*
    // with an `operator[]` — `v[0]` on a `std::vector` — needs the template instantiated, and this answers
    // `Unknown` for the same reason it does everywhere else: see [`element_type_name`].
    if let Some(base) = subscript_base(expression) {
        let base_type = type_of_expression(index, scopes, root, path, &base, depth + 1);
        return match base_type {
            Known::Yes((type_of, file)) => match element_type_name(&type_of) {
                Some(element) => Known::Yes((element, file)),
                None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
            },
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A member access: the type of the member, which is a fact on its declaration.
    if let Some(inner) = crate::sema::resolve::member_access_of(expression) {
        let Known::Yes((inner_type, declared_in)) =
            type_of_expression(index, scopes, root, path, &inner.object, depth + 1)
        else {
            let Known::Unknown(reason) =
                type_of_expression(index, scopes, root, path, &inner.object, depth + 1)
            else {
                unreachable!("the first match established that this is an `Unknown`")
            };
            return Known::Unknown(reason);
        };

        let class = base_type_name(&inner_type);
        if class.is_empty() {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
        }

        // **The class as the declaration spelled it, and then as the name that spelling resolves to.** A type
        // written inside a namespace is spelled there *without* its qualifier — MSVC's `<iostream>` writes
        // `extern istream cin;` inside `std` — so a member lookup for `std::cin.read` is handed `istream` while the
        // class it needs is `std::istream`. C++ resolves that spelling in the scope the declaration is in; this
        // asks the index for the **name** from the file the declaration is in, which is the same lookup whenever
        // that file sees exactly one such name — and no answer at all (with the first reason kept) when it sees
        // several, which is the direction this layer fails in.
        let found = member_fact(index, scopes, root, path, class, &inner.member);
        let found = match found {
            Known::Yes(found) => Known::Yes(found),
            Known::Unknown(reason) => match declared_class(index, &declared_in, class) {
                Some(qualified) => match member_fact(index, scopes, root, path, &qualified, &inner.member) {
                    Known::Yes(found) => Known::Yes(found),
                    Known::Unknown(_) | Known::No => Known::Unknown(reason),
                },
                None => Known::Unknown(reason),
            },
            Known::No => Known::No,
        };
        let Known::Yes((fact, file)) = found else {
            let Known::Unknown(reason) = found else {
                unreachable!("the first match established that this is an `Unknown`")
            };
            return Known::Unknown(reason);
        };

        return match fact.type_of {
            Some(type_of) => Known::Yes((type_of, file)),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    Known::Unknown(UnknownReason::UnknownType(Box::from(written)))
}

/// The type **every** visible declaration of `name` agrees on — the declaration to read it from, and its file.
///
/// The one question a type query can still answer when a *name* query cannot: `Ambiguous` means several
/// declarations are visible, and "which one is it" has no answer, while "what type does it have" does whenever
/// they all say the same thing — which redeclarations do by construction
/// (`extern istream cin;` written twice in MSVC's `<iostream>`, once of them under `extern "C++"`).
///
/// `None` when there is nothing visible, when any candidate is a function or a class (neither has a type *as a
/// name* — see [`DeclFact::returns`]), or when two candidates spell the type differently. A disagreement is
/// exactly what `Ambiguous` was about, so it is not resolved here.
fn agreeing_type(
    index: &ProjectIndex,
    path: &Path,
    name: &str,
) -> Option<(DeclFact, PathBuf)> {
    let candidates = index.files_declaring(name, path);
    let mut agreeing: Option<(DeclFact, PathBuf)> = None;

    for found in &candidates {
        let type_of = found.fact.type_of.as_deref()?;
        match &agreeing {
            None => agreeing = Some((found.fact.clone(), found.file.clone())),
            Some((first, _)) if first.type_of.as_deref() == Some(type_of) => {}
            Some(_) => return None,
        }
    }

    agreeing
}

/// The longer spelling a bare type name resolves to **where it was declared** — `Widget` → `lib::Widget`.
///
/// `None` in the three cases where the spelling as written is all this layer has: it is already qualified (a claim
/// about where the name lives, so there is nothing to look up), nothing declares it, or **more than one**
/// declaration is visible from the declaring file — where the honest answer is the caller's first reason rather
/// than one of the candidates.
///
/// The same one step of the same rule [`resolved_in_the_enclosing_scopes`] applies to a base clause, and the same
/// reason it exists: a name written inside a namespace is spelled without that namespace, and a lookup that stopped
/// at the spelling found `std::_Tree` neither as a base of `std::map` nor as the class of `std::cin`.
fn declared_class(index: &ProjectIndex, declaring: &Path, class: &str) -> Option<String> {
    if class.contains("::") {
        return None;
    }

    match index.definition(class, declaring) {
        Known::Yes(found) => {
            let qualified = found.fact.qualified_name();
            (qualified != class).then_some(qualified)
        }
        Known::Unknown(_) | Known::No => None,
    }
}

/// The operand of the unary expression this node is, when its operator is `operator`.
///
/// `*p` and `&x` are `UnaryExpr` — the node every unary operator gets — so what makes one a dereference is the
/// **operator**, exactly as it is for a member access (`w.size` and `arr[0]` share a node kind too). The operator
/// is matched by text rather than by token kind for the same reason: a shape reader that had to know the token's
/// name would be a second place to update if the name changed.
///
/// `-x` and `!x` are `UnaryExpr`s as well and are deliberately not this: what they have is not a pointer.
fn unary_operand_with(
    node: &cpp_parser::CppSyntaxNode,
    operator: &str,
) -> Option<cpp_parser::CppSyntaxNode> {
    if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::UnaryExpr {
        return None;
    }

    let first = node.children_with_tokens().next()?;
    if first
        .as_token()
        .is_none_or(|token| token.text() != operator)
    {
        return None;
    }

    // The operand is the node after it, which is what a type is inferred *from*.
    node.children().next()
}

/// The base of a `[…]` expression, when that is what this node is.
///
/// `arr[0]` and `w.size` are both `IndexExpr` — the parser reads `w.size` as an index expression with a `.`
/// where the brackets would be — so the operator is
/// what tells them apart. A member access is not a subscript and never reaches the inference for one.
fn subscript_base(node: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxNode> {
    if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::IndexExpr {
        return None;
    }

    let has_brackets = node.children_with_tokens().any(|element| {
        element
            .as_token()
            .is_some_and(|token| token.text() == "[")
    });
    if !has_brackets {
        return None;
    }

    node.children().next()
}

/// What a `*` on this spelling gives: `Widget*` → `Widget`, `Widget&` → `Widget`, `Widget**` → `Widget*`.
///
/// The operators written after the type are removed, and so is a cv-qualifier written after *them* — `Widget *
/// const` is a const pointer to a `Widget`, so what the `*` gives is the `Widget` and not `Widget * const`.
/// Everything else is kept as written, template arguments included: how much of a spelling names the *class* is
/// the member lookup's question, and it answers that one itself with [`base_type_name`].
///
/// `None` when there is no operator to remove, which is the honest answer for `*x` where the declaration says
/// `x` is an `int`: nothing in a declaration of `x` says otherwise, and a type invented here would be a wrong
/// answer rather than a missing one.
fn pointee_type_name(written: &str) -> Option<String> {
    let mut name = written.trim().to_string();

    // The qualifiers of the *pointer* come first because they are written last: `Widget* const`.
    loop {
        let trimmed = name.trim_end();
        let Some(rest) = trimmed
            .strip_suffix("const")
            .or_else(|| trimmed.strip_suffix("volatile"))
        else {
            break;
        };
        name = rest.trim_end().to_string();
    }

    let trimmed = name.trim_end();
    let stripped = trimmed
        .strip_suffix("&&")
        .or_else(|| trimmed.strip_suffix('*'))
        .or_else(|| trimmed.strip_suffix('&'))?;

    let pointee = stripped.trim().to_string();
    (!pointee.is_empty()).then_some(pointee)
}

/// What a subscript on this spelling gives, for the one case this layer can compute: an **array**.
///
/// ```text
/// Widget[4]     →  Widget
/// int[2][3]     →  int[2]      the last `[ … ]` is the one the subscript applies to
/// ```
///
/// `None` for everything else, and the case that matters is a *class*: subscripting a `std::vector<Widget>`
/// gives a `Widget&`, and reaching that means **instantiating** the template rather than reading a spelling.
/// `None` becomes [`UnknownReason::UnknownType`] at the call site, which is the honest answer until that layer
/// exists — the alternative, taking the first template argument of whatever the spelling names, would be right
/// for a `vector` and wrong for a `map`, and nothing here can tell them apart.
fn element_type_name(written: &str) -> Option<String> {
    let trimmed = written.trim_end();
    if !trimmed.ends_with(']') {
        return None;
    }

    // From the right, so that the *last* bracket pair is the one found: `int[2][3]` is an array of arrays.
    let mut depth = 0isize;
    for (index, character) in trimmed.char_indices().rev() {
        match character {
            ']' => depth += 1,
            '[' => {
                depth -= 1;
                if depth == 0 {
                    let element = trimmed[..index].trim();
                    return (!element.is_empty()).then(|| element.to_string());
                }
            }
            _ => {}
        }
    }

    None
}

/// The declaration an expression **names**, from the file being edited or from the index.
///
/// The reconciliation the two-layer split needs, in one place: the buffer answers with a [`crate::Binding`] whose
/// spellings are read out of the tree, while an indexed file answers with a [`DeclFact`] that carries them as
/// fields. Both are needed for the same two questions — what type does this name have, what does a call of it
/// give — and a second reconciliation would be a second rule book for the same question.
///
/// The answers are the ones the name query gives, and for the same reason: a name this file cannot place is asked
/// of the index, and a name *nothing* declares is [`UnknownType`] carrying the spelling rather than
/// [`UnknownReason::NotDeclaredHere`] — what is missing here is the type of an expression, which is a different
/// thing for a consumer to be told.
///
/// [`UnknownType`]: UnknownReason::UnknownType
enum NamedDeclaration {
    /// A binding of the file being edited. Its spellings come from the tree the caller has.
    Here(crate::Binding),
    /// A declaration in an indexed file, with the file it is in.
    Indexed(DeclFact, PathBuf),
}

impl NamedDeclaration {
    /// Where the declaration is — the answer's own file, for a consumer that has to jump or to look further.
    fn file(&self, here: &Path) -> PathBuf {
        match self {
            NamedDeclaration::Here(_) => here.to_path_buf(),
            NamedDeclaration::Indexed(_, file) => file.clone(),
        }
    }

    /// The type this declaration was written with.
    ///
    /// `None` for a function — `make` is not a `Widget` and has no members — which is the distinction
    /// [`DeclFact::type_of`] documents.
    fn type_of(&self, root: &cpp_parser::CppSyntaxNode) -> Option<String> {
        match self {
            NamedDeclaration::Here(binding) => {
                crate::sema::declarations::declared_type_of(root, binding)
            }
            NamedDeclaration::Indexed(fact, _) => fact.type_of.clone(),
        }
    }

    /// What a **call** of this declaration has: the type it returns, or the class it declares.
    ///
    /// Two answers because C++ has two, and the tokens do not separate them: `make()` is a call of a function and
    /// has what it returns, while `Widget()` — the same shape — is a *temporary* of the class. Only the
    /// declaration says which, which is why this is asked here rather than of the shape.
    fn what_a_call_has(&self, root: &cpp_parser::CppSyntaxNode) -> Option<String> {
        match self {
            NamedDeclaration::Here(binding) => {
                if binding.kind == crate::BindingKind::Class {
                    return binding
                        .name
                        .identifier_text()
                        .map(|name| name.to_string());
                }
                crate::sema::declarations::declared_returns_of(root, binding)
            }
            NamedDeclaration::Indexed(fact, _) => what_a_call_has_in(fact),
        }
    }

    /// The offset of the declared **name**, in the file [`NamedDeclaration::file`] answers with.
    ///
    /// A name range rather than the declaration's range, because the two answer different questions: a rename
    /// edits the name, and a caller looking for the declaration's own declarator has to be *at* the name — see
    /// [`callee_of_a_call`].
    fn name_offset(&self) -> usize {
        match self {
            NamedDeclaration::Here(binding) => binding.name_range.start_offset,
            NamedDeclaration::Indexed(fact, _) => fact.name_range.start_offset,
        }
    }
}

/// The type a declaration was written with — or, where it wrote `auto`, the type its **initializer** has.
///
/// # What `auto` means here
///
/// `auto n = count();` declares `n` with the type of `count()`, and until this existed the answer to "what is `n`"
/// was the word `auto` — which is not a type, and which every consumer then had to special-case or give up on:
/// `auto w = Widget{}; w.` listed nothing, hover said `auto`, and the parameter hints could not say what an
/// argument was. The deduction is the language's own rule read off the declaration the file wrote:
///
/// ```text
/// auto n = count();          the initializer's type                          `int`
/// const auto& r = n;         that type with what was written around `auto`    `const int&`
/// auto p = &r;               the initializer is `&r`, so `const int*`         `const int*`
/// auto q = Widget{};         a braced initializer names the type it makes     `Widget`
/// ```
///
/// # What it refuses, and why that is the rule rather than a shortcut
///
/// * **`auto&&`** — the language deduces a reference *or* a value depending on the initializer's value category
///   (`T&` for an lvalue, `T&&` for an rvalue), and nothing in a declaration's spelling says which. Reporting what
///   was written around `auto` would be wrong half the time, so the answer is `Unknown`.
/// * **a `*` the initializer does not have** — `auto* p = x;` where `x` is not a pointer is ill-formed; a type
///   invented to make the declaration work would hide that.
/// * **a declaration in another file** — `auto` at namespace scope in a header is deduced from that header's
///   syntax, and this layer holds one file's tree. The spelling in the index (`auto`) is not a type, so the answer
///   is `Unknown` rather than a guess.
/// * **a chain longer than [`MAX_TYPE_DEPTH`]** — see the guard in [`type_of_expression`].
fn declared_type(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    named: &NamedDeclaration,
    depth: usize,
) -> Known<String> {
    let Some(written) = named.type_of(root) else {
        return Known::No;
    };

    if !writes_auto(&written) {
        return Known::Yes(written);
    }

    let unknown = || Known::Unknown(UnknownReason::UnknownType(Box::from(written.trim())));

    // The **written** spelling, rebuilt from the declaration's syntax rather than taken from the recorded type:
    // the recorded one has already dropped the declaration's specifiers, and `const auto& r = x;` has to deduce a
    // `const` type — see `deduction_inputs`.
    let Some((as_written, _)) = deduction_inputs(root, named) else {
        return unknown();
    };
    if !writes_auto(&as_written) {
        return unknown();
    }

    let Known::Yes((deduced, _)) = initializer_type(index, scopes, root, path, named, depth + 1) else {
        return unknown();
    };

    match auto_substituted(&as_written, &deduced) {
        Some(type_of) => Known::Yes(type_of),
        None => unknown(),
    }
}

/// Does this spelling use the `auto` placeholder? A word, not a substring: `automatic` is a name.
fn writes_auto(written: &str) -> bool {
    written
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| word == "auto")
}

/// The type of what the declaration is initialized with.
///
/// The declaration's own syntax, found by **descending to the binding's range**: a binding's range is its
/// declarator (`n = count()` is the `InitDeclarator`), and the initializer is one of that node's children — the
/// shape the parser gives every initialized declaration, `int x = 0;` and `auto x = f();` alike.
fn initializer_type(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    named: &NamedDeclaration,
    depth: usize,
) -> Known<(String, PathBuf)> {
    let Some((_, expression)) = deduction_inputs(root, named) else {
        // Either the declaration is in another file — the initializer is written where the declaration is, and this
        // layer holds one file's syntax — or it has none at all (`auto n;` is ill-formed, and saying so is the
        // honest answer).
        return Known::Unknown(UnknownReason::UnknownType(Box::from("auto")));
    };

    type_of_expression(index, scopes, root, path, &expression, depth)
}

/// The spelling `auto` stands for, and the expression that decides it.
///
/// # Why the spelling is rebuilt rather than taken from the declaration's recorded type
///
/// [`crate::declared_type_of`] strips declaration **specifiers**, which is right for a lookup by name (`static
/// const Widget` names `Widget`) and wrong for a type a reader is shown: `const auto& r = x;` has to deduce
/// `const int&`, and the `const` is a specifier the recorded spelling has already dropped. So the two halves are
/// read from the syntax instead — the specifier sequence (`const auto`) and the declarator's own operators (`&`) —
/// and put back together the way the file would have spelled the type it let `auto` stand for.
///
/// The declarator's operators are its text with the name taken out: `& r ` is `&`, `* p ` is `*`, `p ` is nothing.
fn deduction_inputs(
    root: &cpp_parser::CppSyntaxNode,
    named: &NamedDeclaration,
) -> Option<(String, cpp_parser::CppSyntaxNode)> {
    let NamedDeclaration::Here(binding) = named else {
        return None;
    };

    let declarator = node_covering(root, binding.name_range.start_offset, binding.range)?;
    let initializer = declarator.children().find(|child| {
        cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::Initializer
    })?;
    let expression = initializer.children().next()?;

    // The specifiers are a sibling of the declarator's parent — `Declaration > [DeclSpecifierSeq, InitDeclarator]`
    // — and they are where the `auto` and its qualifiers are written.
    let specifiers = declarator
        .ancestors()
        .find_map(|node| {
            node.children().find(|child| {
                cpp_parser::CppSyntaxKind::from(child.kind())
                    == cpp_parser::CppSyntaxKind::DeclSpecifierSeq
            })
        })
        .map(|node| node.text().to_string())
        .unwrap_or_default();

    // The name's own text, taken from the file: the declarator's text minus the name is what was written *around*
    // it — the half the recorded type keeps and the specifiers do not. The **declarator** rather than the node the
    // binding ranges over, because that node is the whole `InitDeclarator` (`& r = x`) and its text holds the
    // initializer too.
    let name_range = binding.name_range;
    let name = root
        .text()
        .to_string()
        .get(name_range.start_offset..name_range.end_offset())
        .unwrap_or_default()
        .to_string();
    let operators: String = declarator
        .children()
        .find(|child| {
            cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::Declarator
        })
        .map(|node| node.text().to_string())
        .unwrap_or_default()
        .replace(&name, "")
        .split_whitespace()
        .collect();

    Some((
        format!("{} {}", specifiers.trim(), operators)
            .trim()
            .to_string(),
        expression,
    ))
}

/// The node whose span is exactly `range`, found along the path to `offset`.
///
/// A binding records a range, and the node it names can be several levels down; walking from the root along the
/// child that contains the offset costs the depth of the tree rather than a search of it.
fn node_covering(
    root: &cpp_parser::CppSyntaxNode,
    offset: usize,
    range: cpp_parser::SourceRange,
) -> Option<cpp_parser::CppSyntaxNode> {
    let matches = |node: &cpp_parser::CppSyntaxNode| {
        usize::from(node.text_range().start()) == range.start_offset
            && usize::from(node.text_range().end()) == range.end_offset()
    };

    let mut node = root.clone();
    loop {
        if matches(&node) {
            return Some(node);
        }

        // **Nodes only, and half-open.** The descent follows the child that contains the offset, and two details
        // decide whether it arrives: a token contains the offset just as its node does (taking it would end the
        // walk one level early), and a node's `end` is *exclusive* — so an offset at the boundary between two
        // children belongs to the one that starts there. Both were wrong once: `auto n = …` answered `None`
        // because the name's offset is exactly where the specifier sequence before it ends.
        let next = node
            .children_with_tokens()
            .find(|element| {
                element.as_node().is_some_and(|child| {
                    let start = usize::from(child.text_range().start());
                    let end = usize::from(child.text_range().end());
                    offset >= start && offset < end
                })
            })
            .and_then(|element| element.into_node())?;

        node = next;
    }
}

/// The written spelling with the initializer's type substituted for `auto`.
///
/// What was written *around* `auto` is what a reader sees and what the language applies: the qualifiers in front
/// (`const`), and the declarator's operators behind (`&`, `*`). The `&&` case is refused here — see
/// [`declared_type`] — and each `*` has to match a pointer in the deduced type, because a `*` that does not is a
/// declaration that does not compile rather than a type to report.
fn auto_substituted(written: &str, deduced: &str) -> Option<String> {
    // The `auto` **word**, by position: a spelling can hold those four letters inside a longer name
    // (`automatic`), and splitting on words is what keeps the two apart.
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = written.as_bytes();
    let mut at = 0usize;
    let mut found = None;

    while at < bytes.len() {
        if !is_word(bytes[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && is_word(bytes[at]) {
            at += 1;
        }
        if &written[start..at] == "auto" {
            found = Some((start, at));
            break;
        }
    }

    let (start, end) = found?;
    let prefix = written[..start].trim();
    let suffix = written[end..].trim();

    if suffix.contains("&&") {
        return None;
    }

    let mut base = deduced.trim().to_string();
    for _ in 0..suffix.matches('*').count() {
        base = pointee_type_name(&base)?.to_string();
    }

    let mut type_of = String::new();
    if !prefix.is_empty() {
        type_of.push_str(prefix);
        type_of.push(' ');
    }
    type_of.push_str(base.trim());
    // A `*` and a `&` are glued to what they apply to (`const int*`, `int&`) — the way the file would have spelled
    // the type it let `auto` stand for.
    type_of.push_str(&suffix.replace(' ', ""));

    Some(type_of)
}


fn what_a_call_has_in(fact: &DeclFact) -> Option<String> {
    match fact.kind {
        // `Widget()` is a temporary of `Widget`, so a call of a class has the class. `DeclKind::Type` is exactly
        // "this declaration declares a class-like type", which is the case a call whose callee is a *type* lands in.
        crate::DeclKind::Type => Some(fact.qualified_name()),
        _ => fact.returns.clone(),
    }
}

/// The declaration an expression names, or why it names none. See [`NamedDeclaration`].
/// Is this spelling **a name** — a bare identifier, or a `::`-qualified chain of them (`::Widget` included)?
///
/// The test the type layer asks before it treats an expression as a name, and it is deliberately about the
/// *spelling* rather than about the node's kind: a qualified name and a member access are different nodes and
/// different questions — the second is an object and a member of it, the first is one entity with a scope.
fn writes_a_name(written: &str) -> bool {
    let spelling = written.strip_prefix("::").unwrap_or(written);

    !spelling.is_empty() && spelling.split("::").all(is_an_identifier)
}

/// Is this segment a name a declaration could carry?
fn is_an_identifier(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
}

/// **The offset at which "which name is this" is asked**: the expression's **last identifier**.
///
/// The offset decides the question, because [`crate::sema::resolve::definition_at`] builds a name's spelling by
/// walking its chain *up to the cursor* — a cursor on `a` in `a::b::c` asks about `a::b`, and one on `c` asks about
/// the whole of it. An expression node is the **whole** chain: the user pointed at an entity, and what identifies
/// that entity is all of it (`std::cin`, not the namespace `std`). So the question is asked at the far end, which
/// for a bare name is the offset it always was.
///
/// The last **identifier** rather than the node's end offset, and the difference is a bug this had for one revision:
/// a node's range covers its trailing trivia (`NameExpr` for `w` in `int w = 1;` ends after the space), so the end
/// offset lands *past* the name and the lookup answers `UnparsableName` — every plain name in the file losing its
/// type at once.
fn end_of_the_written_name(expression: &cpp_parser::CppSyntaxNode) -> usize {
    let mut last = None;

    for element in expression.descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        if cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::Identifier {
            last = Some(usize::from(token.text_range().start()));
        }
    }

    last.unwrap_or_else(|| usize::from(expression.text_range().start()))
}

fn declaration_of_expression(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
) -> Known<NamedDeclaration> {
    let offset = end_of_the_written_name(expression);
    let written = expression.text().to_string();
    let written = written.trim();

    match crate::sema::resolve::definition_at(scopes, root, offset) {
        Known::Yes(binding) => Known::Yes(NamedDeclaration::Here(binding)),
        Known::Unknown(UnknownReason::NotDeclaredHere(name)) => match index.definition(&name, path) {
            Known::Yes(found) => Known::Yes(NamedDeclaration::Indexed(found.fact, found.file)),
            // **Several declarations of one name, and one type between them.** A name query answers `Ambiguous`
            // when more than one declaration is visible, and that is right for "where is this declared" — but the
            // question here is *what type it has*, and a type is not ambiguous when every declaration spells it the
            // same way. MSVC's `<iostream>` declares `cin` twice (once plain, once as
            // `_EXPORT_STD extern "C++" … istream cin;`), so `std::cin` was `Ambiguous` and every use of it lost its
            // type: `std::cin.read(…)`, `std::cin.eof()`, a completion after `std::cin.` — measured, 8 offsets in
            // one file. Candidates that **disagree**, and functions (which have no type as a name), keep the
            // `Unknown` that the name query gave.
            Known::Unknown(UnknownReason::Ambiguous(_)) => match agreeing_type(index, path, &name) {
                Some(found) => Known::Yes(NamedDeclaration::Indexed(found.0, found.1)),
                None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
            },
            // The declaration is nowhere this analysis can see, so there is no type to read. Reporting the *name*
            // reason would say "the owner is missing" where what is missing is the type of an expression — a
            // different answer for a consumer deciding what to tell the user.
            Known::Unknown(_) | Known::No => {
                Known::Unknown(UnknownReason::UnknownType(Box::from(written)))
            }
        },
        // A name this layer cannot place at all is not a type it can read. `No` means the offset is not on a name;
        // any other `Unknown` is already the most specific answer available and is passed through.
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    }
}

/// The type of a **call**: `make().size` needs this, and so does `fac.build().size`.
///
/// # The two callees, and the one lookup behind them
///
/// ```text
/// make()          a name: what it returns
/// Widget()        a name, and the same tokens — a temporary of the class it names
/// fac.build()     a member access: what the member returns
/// ns::make()      a qualified name: what it returns
/// ```
///
/// The first two are one case because only the *declaration* separates them, and the last two go through the
/// lookups that already exist — the member lookup for a member call, the name lookup for everything else. What
/// this adds is the last step: the callee's declaration says what a call of it has ([`what_a_call_has_in`]).
///
/// Everything else stays [`UnknownReason::UnknownType`]: a call of a function pointer, of a lambda, of a template
/// parameter whose type is not known here. Each of those needs a type *computed* rather than read off a
/// declaration, which is the same boundary the rest of this function has.
fn type_of_a_call(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    call: &cpp_parser::CppSyntaxNode,
) -> Known<(String, PathBuf)> {
    let written = call.text().to_string();
    let written = written.trim();

    let Some(callee) = call.children().next() else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    };

    match declaration_of_a_callee(index, scopes, root, path, &callee, written) {
        Known::Yes(named) => match named.what_a_call_has(root) {
            Some(type_of) => Known::Yes((type_of, named.file(path))),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        },
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    }
}

/// The declaration a **callee expression** names — `make` in `make()`, `fac.build` in `fac.build()`.
///
/// The two lookups a call needs, in one place: a member access goes through the member lookup (the object's type,
/// then the member in it), everything else through the name lookup. The callers differ only in what they then ask
/// of the declaration — [`type_of_a_call`] wants what a call of it *has*, a parameter hint wants the parameters
/// it declares — and neither is a reason for a second copy of the resolution.
///
/// `written` is the **call's** spelling rather than the callee's, and it is what the `Unknown` answers carry: a
/// consumer reporting "not known" should name the code the user wrote, not the half of it the lookup started at.
fn declaration_of_a_callee(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    callee: &cpp_parser::CppSyntaxNode,
    written: &str,
) -> Known<NamedDeclaration> {
    // A member call: the object's type, then the member's declaration.
    if let Some(access) = crate::sema::resolve::member_access_of(callee) {
        let object = match type_of_expression(index, scopes, root, path, &access.object, 0) {
            Known::Yes((type_of, _)) => type_of,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };

        let class = base_type_name(&object);
        if class.is_empty() {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
        }

        return match member_fact(index, scopes, root, path, class, &access.member) {
            Known::Yes((fact, file)) => Known::Yes(NamedDeclaration::Indexed(fact, file)),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // Everything else is a name — `make()`, `Widget()`, `ns::make()` — and the name lookup is the one every other
    // question about a name goes through.
    declaration_of_expression(index, scopes, root, path, callee)
}

/// Where a call's callee is declared — what a parameter hint is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Callee {
    /// The file the declaration is written in.
    pub file: PathBuf,
    /// The offset of the declared **name** in that file.
    pub name_offset: usize,
}

/// The declaration a call names, as **a place in a file**: the file it is in, and the offset of its name.
///
/// [`type_of_a_call`]'s question asked for a different answer: "what does a call of this have" needs the
/// declaration's return type, while a parameter hint needs the parameters it declares — and both need the
/// declaration found the same way.
///
/// Nothing is refused here for not being a function. `Widget(1, 2)` and `fp(1)` are calls whose declaration is a
/// class or a variable, and whether either has a parameter list is a question about the *declaration's own
/// declarator*, which the caller asks: a class name has no declarator at all, and a function pointer's parameters
/// belong to its type rather than to the name. Answering "here is the declaration" and letting that reading say
/// "no parameters" keeps one rule instead of two.
///
/// # Why the *name's* offset rather than the declaration's first token
///
/// A caller looking for the parameters finds them in the declarator the name is a name *of*, so it has to start at
/// the name: a `const`, a `static` or a return type is outside the declarator, and a declaration may hold more
/// than one declarator (`void (*f(int a))(int b)`).
pub(crate) fn callee_of_a_call(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    call: &cpp_parser::CppSyntaxNode,
) -> Known<Callee> {
    let written = call.text().to_string();
    let written = written.trim();

    let Some(callee) = call.children().next() else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    };

    match declaration_of_a_callee(index, scopes, root, path, &callee, written) {
        Known::Yes(named) => Known::Yes(Callee {
            file: named.file(path),
            name_offset: named.name_offset(),
        }),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    }
}

/// The declaration of `member` in the class `class` names, and the file it is in.
///
/// The lookup is the qualified name `<class>::<member>`, which is why nothing here needs to know what a class
/// *is*: the file being edited is asked first through its scope tree, and the index second — the same two-layer
/// split [`definition_across_files`] makes, and for the same reason.
fn member_fact(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
) -> Known<(DeclFact, PathBuf)> {
    // Followed **before** anything else, so that both the lookup and the spelling this answer reports when it
    // fails name the class rather than the alias: `Widget::size` is the question that could not be answered, and
    // `Alias::size` would be a question nobody asked. `direct_member` follows it again for the bases it is handed,
    // where the step is idempotent — a class resolves to itself.
    let class = &resolve_aliases(index, scopes, root, path, class);

    if let Some(found) = direct_member(index, scopes, root, path, class, member) {
        return Known::Yes(found);
    }

    // Inherited members, **level by level**: a member of a direct base hides a member of that base's own base,
    // which is what C++ does, so the search stops at the first level that has any answer. Two answers at the same
    // level are ambiguous — a diamond where both sides declare the name — and reporting that is the honest
    // outcome: picking one would be a jump to an entity the language says is not uniquely named.
    //
    // As in `members_of`, each base is carried with the class whose base-clause wrote it, because that decides the
    // scope an unqualified base name is looked up in — `struct map : _Tree<…>` in `std` names `std::_Tree`.
    let mut level: Vec<(String, String)> = bases_of(index, scopes, root, path, class)
        .value()
        .unwrap_or_default()
        .into_iter()
        .map(|base| (base, class.to_string()))
        .collect();
    let mut visited: Vec<String> = vec![class.to_string()];

    while !level.is_empty() {
        let mut found: Vec<(DeclFact, PathBuf)> = Vec::new();
        let mut next: Vec<(String, String)> = Vec::new();

        for (written, owner) in level {
            let base = resolved_in_the_enclosing_scopes(index, scopes, path, &owner, &written);
            if visited.contains(&base) {
                continue;
            }
            visited.push(base.clone());

            // A base that cannot be resolved contributes nothing *and* stops nothing: whatever it might inherit
            // from is unknown, not absent, and the levels below it are still worth asking.
            if let Some(member_found) = direct_member(index, scopes, root, path, &base, member) {
                found.push(member_found);
            }
            next.extend(
                bases_of(index, scopes, root, path, &base)
                    .value()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|base_of_base| (base_of_base, base.clone())),
            );
        }

        match found.len() {
            0 => level = next,
            1 => return Known::Yes(found.remove(0)),
            _ => return Known::Unknown(UnknownReason::Ambiguous(Box::from(member))),
        }
    }

    Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(format!(
        "{class}::{member}"
    ))))
}

/// How many aliases deep a type spelling is followed before the walk gives up.
///
/// `using A = A;` and `using A = B; using B = A;` are both writable, and a walk that trusted the spelling would
/// follow them for ever. Eight is far past any real chain of aliases — the standard library's deepest are two or
/// three (`string` → `basic_string`, `size_type` → `size_t` → …) — and the answer when it is reached is the
/// spelling that was last resolved, which the caller then reports as not declared rather than as a wrong class.
const MAX_ALIAS_DEPTH: usize = 8;

/// The class a type spelling names, following `typedef`/`using` **aliases**.
///
/// `std::string` is `basic_string<char>`; nobody declares `substr` *in* `string`, because an alias has no members
/// of its own. So a spelling that resolves to an alias is replaced by the type the alias points at, and the
/// lookup carries on with that — which is one step, not a type system: no instantiation, no substitution, no
/// inference of what the target's template arguments mean.
///
/// # The target is resolved in the alias's own scope
///
/// ```cpp
/// namespace std { typedef basic_string<char> string; }   // the target is written *relative to* `std`
/// ```
///
/// Following it as a bare `basic_string` would look in the wrong place. The candidates are therefore
/// `<the alias's scope>::<target>` first and the bare target second — which is C++'s own enclosing-scope lookup,
/// done one step of it: the standard library's aliases are nearly all spelled this way, so the first candidate is
/// the one that answers, and a target that names something global still resolves through the second.
///
/// # Bounded, and honest when it stops
///
/// A spelling that cannot be resolved to a class comes back **unchanged**, so the caller reports
/// `NotDeclaredHere(<what the file wrote>)` rather than a name nobody wrote. See [`MAX_ALIAS_DEPTH`] for the bound
/// and why a bound is needed at all.
fn resolve_aliases(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> String {
    let mut current = class.to_string();
    let mut seen: Vec<String> = Vec::new();

    for _ in 0..MAX_ALIAS_DEPTH {
        if seen.contains(&current) {
            break;
        }
        seen.push(current.clone());

        let Some((target, scope)) = alias_target_of(index, scopes, root, path, &current) else {
            break;
        };

        let target = base_type_name(&target).to_string();
        if target.is_empty() {
            break;
        }

        // An unqualified target is written relative to the scope the alias was declared in, so that spelling is
        // tried first — and the bare one is kept for a target that names a global. A qualified target is already a
        // complete spelling and is taken as it stands.
        let candidates: Vec<String> = match &scope {
            Some(prefix) if !target.contains("::") => {
                vec![format!("{prefix}::{target}"), target.clone()]
            }
            _ => vec![target.clone()],
        };

        let next = candidates
            .iter()
            .find(|candidate| is_declared(index, scopes, path, candidate))
            .unwrap_or(&candidates[0])
            .clone();

        if next == current {
            break;
        }
        current = next;
    }

    current
}

/// What an alias points at, and the scope it was declared in, or `None` when the spelling is not an alias.
///
/// # How an alias is recognised
///
/// In the buffer, by the binding's kind, which is exact. Through the index, by the rule [`DeclFact::type_of`]
/// documents: a **type** fact with a `type_of` is an alias, and a class is a type fact without one. That is the
/// whole of the test, and it is one test rather than a new field on purpose — the field was already there for
/// variables, and what an alias *has* is a type in the same sense: the one it points at.
fn alias_target_of(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Option<(String, Option<String>)> {
    // The buffer first: the alias may be in a file that has never been written to disk.
    let (scope_name, short) = match class.rsplit_once("::") {
        Some((prefix, short)) => (prefix.to_string(), short.to_string()),
        None => (String::new(), class.to_string()),
    };

    if let Some(scope) = scopes.scope_with_qualified_name(&scope_name)
        && let Some(data) = scopes.scope(scope)
        && let Some(binding) = data
            .bindings
            .iter()
            .find(|binding| binding.name.identifier_text() == Some(short.as_str()))
        && let Some(target) = crate::sema::declarations::declared_alias_target(root, binding)
    {
        return Some((
            target,
            (!scope_name.is_empty()).then_some(scope_name.clone()),
        ));
    }

    match index.definition(class, path) {
        Known::Yes(found) if found.fact.kind == crate::DeclKind::Type => found
            .fact
            .type_of
            .clone()
            .map(|target| (target, found.fact.scope.clone())),
        Known::Yes(_) | Known::Unknown(_) | Known::No => None,
    }
}

/// Is this spelling a declaration this analysis can see?
///
/// Used to choose between the two ways of reading an alias's target — relative to the alias's scope, or as a
/// spelling in its own right — and deliberately a *cheap* question: a scope in the buffer or one fact in the
/// index is answered without walking anything.
fn is_declared(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    path: &Path,
    spelling: &str,
) -> bool {
    if scopes.scope_with_qualified_name(spelling).is_some() {
        return true;
    }

    matches!(index.definition(spelling, path), Known::Yes(_))
}

/// The declaration of `member` written **directly** in `class`, or `None`.
///
/// The qualified name `<class>::<member>`, which is why nothing here needs to know what a class *is*: the file
/// being edited is asked first through its scope tree, and the index second — the two-layer split
/// [`definition_across_files`] makes, for the same reason.
fn direct_member(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
) -> Option<(DeclFact, PathBuf)> {
    // An alias has no members of its own, so the spelling is followed to the class it names before anything is
    // looked up: `std::string`'s members are `std::basic_string`'s. This is the one place a spelling becomes a
    // class for a **single** member, which is why the step lives here rather than at every caller.
    let class = &resolve_aliases(index, scopes, root, path, class);

    // A class declared in this file is looked up here first, which is what makes the whole query work on a buffer
    // that has never been written to disk. The declared type is filled in from the tree, because a member of a
    // member is exactly what a nested access asks for next.
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(scope_data) = scopes.scope(scope)
        && let Some(binding) = scope_data
            .bindings
            .iter()
            .find(|binding| binding.name.identifier_text() == Some(member))
    {
        return Some((fact_from_binding(root, class, binding), path.to_path_buf()));
    }

    // Several declarations of one name **in one class** are overloads, not an ambiguity. The language keeps them in
    // a single overload set and picks by argument types, which this layer does not have — and a cursor still needs
    // one answer, so the first in declaration order is it. That is the rule `definition_at` already uses within a
    // scope, and it is why [`ProjectIndex::declarations_in`] is asked here rather than
    // [`ProjectIndex::definition`], whose "several visible declarations" answer is right for a *name* and wrong
    // for a member: measured on the closure of `<string>`, every real member (`size`, `find`, `substr`) has
    // several declarations, so a member lookup that called that ambiguous reported `NotDeclaredHere` for the whole
    // standard library.
    //
    // A consumer that wants all of them asks [`members_of`], which is the query that exists for it — the split is
    // "a jump takes the first, a list takes them all".
    if let Some(found) = index
        .declarations_in(class, path)
        .into_iter()
        .find(|declaration| declaration.fact.name == member)
    {
        return Some((found.fact.clone(), found.file.clone()));
    }

    // Nothing is scoped to that class, so the last thing to try is the qualified spelling itself: a member defined
    // out of line can have been filed under a spelling the class's own scope is not (`C::f` for a definition
    // written outside `C`), and that is exactly what this lookup is for.
    match index.definition(&format!("{class}::{member}"), path) {
        Known::Yes(found) => Some((found.fact, found.file)),
        Known::Unknown(_) | Known::No => None,
    }
}

/// One binding as a [`DeclFact`], under the qualified name of the scope it was written in.
///
/// The single reader of a binding's fact-shaped fields, shared by the single-member query and the member-list
/// query so that `Widget::size`'s type and `Widget`'s member `size` cannot come out differently: both are read
/// out of the tree through [`declared_type_of`](crate::sema::declarations::declared_type_of) and
/// [`declared_bases_of`](crate::sema::declarations::declared_bases_of), and a second call site would be a second
/// answer to "what does this declaration say".
///
/// The guard is [`FactGuard::Unconditional`] because a binding carries none: the region a declaration sits in is
/// a fact about the text, and the layer that sweeps the directives fills it in when the summary is built — see
/// [`build_facts`](crate::sema::declarations::build_facts).
///
/// # The two fields that are not answered on this path
///
/// A fact built from the **buffer's** scope tree has had no directive sweep and has no diagnostic list, so two of
/// its fields are defaults rather than answers: `guard` says `Unconditional` even for a member written inside an
/// `#if`, and `clean` says `true` even in a buffer that does not parse. Both are gaps rather than decisions, and
/// they are stated here rather than left to be discovered: this path is handed a **node** and the tree's
/// diagnostics are not on it, so a caller that needs either field has to ask the summary — which does have them —
/// and a consumer showing a member list has nothing to gain from either, which is why neither was worth
/// threading the tree through every query for. The same applies to the facts [`member_across_files`] returns
/// from this path.
fn fact_from_binding(root: &cpp_parser::CppSyntaxNode, class: &str, binding: &crate::Binding) -> DeclFact {
    DeclFact {
        name: binding
            .name
            .identifier_text()
            .unwrap_or_default()
            .to_string(),
        // `None` for an empty spelling, which is what a caller listing the names of a **body** passes: a local has
        // no qualified scope at all, and `Some("")` would be a scope that matches nothing while looking like one.
        // Every other caller passes the class or namespace the binding is in.
        scope: (!class.is_empty()).then(|| class.to_string()),
        // Answered rather than defaulted, and the answer is always `false`: this path exists for **members**, and a
        // member is declared in a class body by construction — the `class` spelling it is keyed by has no meaning
        // inside a function. A local class's members are members of that class, and the caller that asked for them
        // asked by name. See [`DeclFact::local`].
        local: false,
        kind: crate::DeclKind::from_binding_kind(binding.kind),
        type_of: crate::sema::declarations::declared_type_of(root, binding),
        // Answered here as well, because **members** are exactly where a call happens: `fac.build().size` needs the
        // return type of a member function that the file being edited declares. See [`DeclFact::returns`].
        returns: crate::sema::declarations::declared_returns_of(root, binding),
        bases: crate::sema::declarations::declared_bases_of(root, binding),
        range: binding.range,
        name_range: binding.name_range,
        clean: true,
        guard: FactGuard::Unconditional,
    }
}

/// The base classes `class` was written with, from this file or from the index.
///
/// # Names, not spellings
///
/// What comes back is normalized for **lookup**: `public Base<int>` is the class `Base`. The spelling the file
/// wrote stays in [`DeclFact::bases`], which is a fact about the text; a base here is a name to find a class by,
/// and the template arguments say which *type* is inherited rather than which class declares the members. The
/// members of `Base<int>` are the members of `Base`'s primary template, which is the most useful answer available
/// without instantiating anything — and the same rule [`base_type_name`] applies to a declared type.
///
/// # Why this is `Known` and not a list
///
/// "This class has no bases" and "nothing here says what this class inherits from" are different statements, and
/// an empty `Vec` would merge them. The member **lookup** treats them alike — a base it cannot reach contributes
/// no members either way, so it walks on — while the member **list** reports the gap, because a list is a claim
/// about what a type has.
fn bases_of(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<String>> {
    // In this file: the class scope's parent holds the binding of the class's *name*, which is the declaration the
    // bases were written on. Asking the tree through that binding is the same walk the fact builder makes, so the
    // two cannot disagree about what a class inherits from.
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(data) = scopes.scope(scope)
        && let Some(parent) = data.parent
        && let Some(name) = class.rsplit("::").next()
        && let Some(binding) = scopes
            .scope(parent)
            .and_then(|scope| {
                scope
                    .bindings
                    .iter()
                    .find(|binding| binding.name.identifier_text() == Some(name))
            })
    {
        return Known::Yes(lookup_names(&crate::sema::declarations::declared_bases_of(
            root, binding,
        )));
    }

    match index.definition(class, path) {
        Known::Yes(found) => Known::Yes(lookup_names(&found.fact.bases)),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::No,
    }
}

/// Base spellings as names a class can be looked up by, in the order they were written.
fn lookup_names(bases: &[String]) -> Vec<String> {
    bases
        .iter()
        .map(|base| base_type_name(base).to_string())
        .filter(|base| !base.is_empty())
        .collect()
}

/// The spelling a base-clause name resolves to, given the class it was written in.
///
/// A base name is looked up **from the scope enclosing the class**, outward — so `_Tree` written in
/// `namespace std { class map : _Tree<…> }` names `std::_Tree`, and a `_Tree` at file scope could not answer it
/// even if there were one. Both base walks used to look for the spelling exactly as written, and MSVC's `<map>`
/// inherits from `_Tree`, declared in `<xtree>` inside `_STD_BEGIN` (= `namespace std {`): the member list reported
/// `UnlistedBase { "_Tree", NotDeclaredHere }` while `std::_Tree` sat in the index with 126 members, four of them
/// `find` — which is what the last two queries of `examples/std_query.rs` were failing on (measured).
///
/// The enclosing scopes are tried innermost first and **the spelling as written is the last candidate**, which is
/// what the rule says (the global name space is the outermost scope) and also what keeps a base named at file
/// scope — `struct Derived : public Base` — answering as it always did.
///
/// A base that already carries a `::` is returned untouched: a qualified name is a claim about where the name
/// lives, and `std::_Tree` written in a class in `std` means that name and no other. A **leading** `::` is such a
/// claim too, and `base_type_name` has already taken it off; the name it leaves is the global one, which is what
/// the last candidate below is.
///
/// The same one step of the same rule is what [`resolve_aliases`] does for an alias's target and documents there;
/// both ask it through [`is_declared`], so "the buffer first, the index second" is not decided twice.
fn resolved_in_the_enclosing_scopes(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    path: &Path,
    owner: &str,
    base: &str,
) -> String {
    if base.contains("::") {
        return base.to_string();
    }

    let mut enclosing = owner;
    while let Some((outer, _)) = enclosing.rsplit_once("::") {
        let candidate = format!("{outer}::{base}");
        if is_declared(index, scopes, path, &candidate) {
            return candidate;
        }
        enclosing = outer;
    }

    base.to_string()
}

/// The class whose scope encloses `offset`, for `this`.
///
/// The nearest class-like scope on the chain, which is the same answer in a member function, in a nested class's
/// member function (the nested class wins, correctly) and in a default member initialiser.
fn enclosing_class(scopes: &crate::ScopeTree, offset: usize) -> Option<String> {
    let innermost = scopes.scope_at(offset)?;

    scopes
        .scope_chain(innermost)
        .into_iter()
        .find_map(|scope| {
            let data = scopes.scope(scope)?;
            // One variant for class, struct and union: the difference is access, which is a member's property
            // rather than the scope's. See `ScopeKind`.
            (data.kind == crate::ScopeKind::Class)
                .then(|| scopes.qualified_name_of(scope))
                .flatten()
        })
}

/// The name of the class a written type names, with the parts that do not affect *which* class it is removed.
///
/// `const Widget&` → `Widget`, `std::vector<int>` → `std::vector`, `struct Widget` → `Widget`, `::Widget` →
/// `Widget`. The goal is a spelling that can be looked up as a qualified name, and every one of those parts is
/// about the type's shape or about which name space it is in rather than about its name. `unsigned long` is left
/// alone: there the words *are* the type, and it names no class anyway.
///
/// # Why the leading `::` comes off
///
/// A leading `::` asks about the **global** name space — a real distinction for a *name* lookup, where dropping it
/// would also match `ns::Widget`, and [`matches`] honours it for exactly that reason. It is not a distinction for
/// this walk, because the walk does not ask "which name space"; it asks for the qualified spelling the index keys
/// on, and a global declaration's spelling is its bare name. Keeping the prefix made `::Widget w;` — the type
/// spelling a consumer hands over verbatim from `DeclFact.type_of` — fail to find a class sitting in the buffer.
pub(crate) fn base_type_name(written: &str) -> &str {
    let mut name = written.trim();

    // Template arguments: the members of `std::vector<int>` are the members of `std::vector`'s primary template,
    // which is the most useful answer available without instantiating anything.
    if let Some(position) = name.find('<') {
        name = name[..position].trim();
    }

    // Declarators written after the type: `Widget*`, `Widget&`, `Widget&&`.
    name = name.trim_end_matches(['*', '&']).trim();

    // The global name space, which for a qualified spelling is no prefix at all.
    name = name.strip_prefix("::").unwrap_or(name).trim();

    // An elaborated specifier: `struct Widget` and `Widget` name one class, and only the second is a spelling the
    // index matches.
    for keyword in ["struct ", "class ", "union ", "enum "] {
        if let Some(rest) = name.strip_prefix(keyword) {
            name = rest.trim();
            break;
        }
    }

    name
}


impl ProjectIndex {
    pub fn new() -> Self {
        ProjectIndex {
            // **Incomplete**, so that a name this index has not been told about is `Unknown` rather than "not
            // defined". An index built by hand has been told nothing, and the default has to be the answer that
            // claims nothing: `Marked::default()` on its own would decide every `#ifdef` in every file as false.
            macros: Marked::default().incomplete(),
            visibility_answers: std::sync::Mutex::new(HashMap::new()),
            ..ProjectIndex::default()
        }
    }

    /// Tell the index what the **compilation** defines: the compiler's predefined names, then the command line's
    /// `-D`s.
    ///
    /// One environment for the whole index, which is an approximation the caller should know about: a project
    /// whose files are built with different `-D`s has one environment per *target*, and this models the one the
    /// caller supplies. Recorded rather than guessed at here.
    ///
    /// # Complete or not, and who decides
    ///
    /// The state's own [`Marked::incomplete`] flag is **kept as given**, and that flag is the difference between
    /// two answers to `#ifdef NAME` when nothing the index read defines `NAME`:
    ///
    /// ```text
    /// incomplete  → Unknown  — a file nobody read, or a `-D` nobody mentioned, could define it
    /// complete    → false    — within this compilation, nothing defines it, and the branch is not taken
    /// ```
    ///
    /// The second is what makes `#ifndef NT_INCLUDED / #include <winnt.h> / #endif` decide itself, and it is a
    /// claim about the **inputs**: the caller is saying "these are the compilation's own definitions, and every
    /// `#include` that matters resolved and was indexed". [`crate::Session`] says it when the compiler it discovered
    /// **answered with its predefined macros** — the table `-dM`/`/PD` prints, and the only place the compiler's own
    /// names exist — and a walk that runs into an `#include` it cannot read takes the claim back
    /// ([`Marked::mark_incomplete`]). See `Session::assemble` for what tying the claim to a compile database instead
    /// cost (MSVC's whole standard library, measured).
    ///
    /// A name a *conditional* might define is a smaller doubt and is handled per name: the walk records it with
    /// [`Marked::mark_uncertain`], so `#ifdef` on it is `Unknown` even in a complete environment.
    pub fn with_macros(mut self, macros: Marked) -> Self {
        self.macros = macros;
        self
    }

    /// The macros a compilation starts with — see [`ProjectIndex::with_macros`].
    pub fn macros(&self) -> &Marked {
        &self.macros
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
        // A new summary can change any condition's answer, so the memo goes: it is cheap to lose and a wrong
        // answer is not.
        if let Ok(mut answers) = self.visibility_answers.lock() {
            answers.clear();
        }
        let mut summary = summary;
        summary.path = path.to_path_buf();

        let path = normalize(path);

        // Remove the edges the previous version of this file contributed, so that a deleted `#include` stops
        // making its target reachable. Without this, an edge would outlive the line that wrote it.
        let sequence = if let Some(previous) = self.summaries.get(&path) {
            let sequence = self.sequence[&path];

            for target in include_targets(previous) {
                if let Some(includers) = self.included_by.get_mut(&target) {
                    includers.remove(&path);
                }
            }

            // The names the previous text declared and the macros it defined stop being findable under this file.
            self.names.remove_declarations(sequence, false, &previous.declarations);
            self.names.remove_macros(sequence, &previous.macros);

            // **The cooked reading describes the file *under the key it was built with***, so a summary filed under
            // a different key takes it with it. Both halves of the key are the reason: a different content hash means
            // the text changed, and a different context hash means the *compilation* did — the configuration or the
            // directory a relative include resolves against — and a rendering is a function of both.
            //
            // `forget` already drops the summary and the reading together, and that is the ordinary path (an edit, a
            // watched file, a close). This one covers the paths that have no `forget` in them: a re-read that found
            // different bytes, a cache entry replaced by a fresh index, and the whole-project re-read a changed
            // `compile_commands.json` causes. Leaving the reading there would answer for a file nobody has any more,
            // at offsets into words that are gone — a wrong answer rather than a missing one.
            if previous.key != summary.key
                && let Some(cooked) = self.cooked.remove(&path) {
                    self.names.remove_declarations(sequence, true, &cooked.declarations);
                }

            sequence
        } else {
            let sequence = self.next_sequence;
            self.next_sequence += 1;
            self.order.insert(sequence, path.clone());
            self.sequence.insert(path.clone(), sequence);

            // A reading filed before its summary (the contract says it should not be, but `insert_cooked` does not
            // refuse) is findable from the moment the file has a number to be found under.
            if let Some(cooked) = self.cooked.get(&path) {
                self.names.add_declarations(sequence, true, &cooked.declarations);
            }

            sequence
        };

        for target in include_targets(&summary) {
            self.included_by
                .entry(target)
                .or_default()
                .insert(path.clone());
        }

        self.names.add_declarations(sequence, false, &summary.declarations);
        self.names.add_macros(sequence, &summary.macros);
        self.summaries.insert(path, summary);
    }

    /// Forget the file at `path`, and the edges its summary contributed.
    ///
    /// The one operation that makes a *deleted* file stop answering. Without it a query would go on finding
    /// declarations in a file that is no longer on disk — the summary is in memory, and nothing about it is
    /// wrong except that it describes something that is gone. The reverse edges go with it, for the same reason
    /// they are removed when a file is re-indexed: an edge outliving the line that wrote it would keep a header
    /// reachable from a file that no longer includes it.
    ///
    /// Returns whether there was anything to forget, which is what lets a watcher report "this event changed
    /// nothing" rather than counting every ignored path as work.
    pub fn forget(&mut self, path: &Path) -> bool {
        let path = normalize(path);
        let Some(summary) = self.summaries.remove(&path) else {
            return false;
        };

        for target in include_targets(&summary) {
            if let Some(includers) = self.included_by.get_mut(&target) {
                includers.remove(&path);
            }
        }
        let sequence = self.sequence.remove(&path);
        if let Some(sequence) = sequence {
            self.order.remove(&sequence);
            self.names.remove_declarations(sequence, false, &summary.declarations);
            self.names.remove_macros(sequence, &summary.macros);
        }
        // **The cooked reading goes with it.** A file that is no longer indexed must not keep answering
        // declaration queries out of a rendering nobody has any more — the same rule as the summary, for the
        // same reason: an answer about a file that is gone is worse than no answer.
        if let Some(cooked) = self.cooked.remove(&path)
            && let Some(sequence) = sequence {
                self.names.remove_declarations(sequence, true, &cooked.declarations);
            }

        true
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
        self.order.values().filter_map(|path| self.summaries.get(path))
    }

    /// The files that include `path`, directly.
    pub fn includers_of(&self, path: &Path) -> Vec<PathBuf> {
        self.included_by
            .get(&normalize(path))
            .map(|includers| includers.iter().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    /// **Add what a file's cooked reading found** — see the `cooked` field for what it is and why only declarations
    /// and diagnostics.
    ///
    /// The file must already be in the index ([`ProjectIndex::insert`]): a cooked reading is a *second* answer
    /// about a file the index knows, and its visibility is the file's own — working that out again per query would
    /// be a second include-graph walk for an answer already computed.
    ///
    /// Inserting **replaces** what was there: a file that was cooked again (its text changed, or its environment
    /// did) must not keep the old reading's declarations beside the new one's.
    pub fn insert_cooked(&mut self, path: &Path, reading: CookedFile) {
        let path = normalize(path);
        // The memo keyed by `(file, region)` is cleared for the same reason `insert` clears it: an answer about
        // visibility can only change when what the index holds changes.
        self.visibility_answers
            .lock()
            .expect("the visibility memo is not poisoned")
            .clear();

        if let Some(&sequence) = self.sequence.get(&path) {
            if let Some(previous) = self.cooked.get(&path) {
                self.names.remove_declarations(sequence, true, &previous.declarations);
            }
            self.names.add_declarations(sequence, true, &reading.declarations);
        }
        self.cooked.insert(path, reading);
    }

    /// The declarations the file at `path` was **cooked** into, when it was cooked at all.
    pub fn cooked_declarations(&self, path: &Path) -> Option<&[DeclFact]> {
        Some(self.cooked_reading(path)?.declarations.as_slice())
    }

    /// **The whole cooked reading** of the file at `path`: what a compiler sees declared there, what the parse of
    /// that text reported, and how much of it this file cannot show — see [`crate::CookedFile`].
    ///
    /// `None` when the file has no cooked reading, which is not the same as "nothing to say": it means *this
    /// reading* has not been made, and a caller that needs an answer falls back to the file's own text
    /// ([`crate::Session::diagnostics`] does exactly that).
    pub fn cooked_reading(&self, path: &Path) -> Option<&CookedFile> {
        self.cooked.get(&normalize(path))
    }

    /// Forget what a file **was read as**, keeping its summary.
    ///
    /// The invalidation a cooked reading needs and a summary does not: a file's summary is a reading of its own text
    /// (so it changes when the text does — [`ProjectIndex::forget`]), while its cooked reading is a reading of its
    /// text **and of its environment**, so it also changes when something the file includes changes — and nothing
    /// about this file's text says so.
    ///
    /// Dropping rather than marking: between the change and the next cook, a query that found the old declarations
    /// would answer with a declaration whose range points into text that no longer describes it. Not found is the
    /// honest answer there, and the reading comes back one drain later.
    pub fn forget_cooked(&mut self, path: &Path) {
        let path = normalize(path);
        if let Some(cooked) = self.cooked.remove(&path)
            && let Some(&sequence) = self.sequence.get(&path) {
                self.names.remove_declarations(sequence, true, &cooked.declarations);
            }
    }

    /// **The project's symbols matching `query`**, best first — what a `workspace/symbol` search shows.
    ///
    /// # What is searched, and what is not
    ///
    /// Every declaration in every summary the index holds, **plus the names only the cooked reading declares**: the
    /// same two readings every other declaration query unions, asked here **without the visibility walk**, because a
    /// workspace search is about the project and not about what one file can see. A name a macro declared
    /// (`DECLARE_HANDLE(HWND)`'s `HWND__`) is a symbol a compiler knows, so a search finds it; a declaration both
    /// readings found is one answer, deduplicated the same way (`(qualified name, kind)` per file).
    ///
    /// **Locals are left out** ([`DeclFact::local`]): a variable inside a function body is not a project symbol, and
    /// a summary cannot even place it in the function it belongs to.
    ///
    /// # The match, and what the cap means
    ///
    /// Case-insensitive **substring of the name**, so `wid` finds `ns::Widget`. The ranking is exact name, then a
    /// prefix of the name, then anything — and within a rank, alphabetical **by name** and then by qualified name,
    /// because two runs over one project have to answer in the same order or a diff of two answers is noise.
    ///
    /// # Why the order is by name, and what that buys
    ///
    /// The names are kept sorted ([`crate::index::names`]), so an answer in name order is one the search can stop
    /// producing as soon as it has `limit` of them: the cost is the size of the answer, not the size of the project.
    /// (Ordering by the *qualified* name — which puts `a::widget` before `widget` before `b::widget` — would need
    /// every candidate's scope before the first could be placed.) Two declarations of one name are still adjacent,
    /// ordered by where they are.
    ///
    /// `limit` caps the answer, and hitting it is **not** an error: a search box is not a claim about the project
    /// the way a reference list is — the user narrows the query, and every editor's symbol search truncates. That is
    /// the opposite of the decision [`crate::macro_references`] makes, where a partial answer would be a false one.
    pub fn symbols_matching(&self, query: &str, limit: usize) -> Vec<ProjectSymbol> {
        let wanted = query.trim().to_lowercase();
        let bare = wanted.trim_start_matches("::");
        if bare.is_empty() || limit == 0 {
            return Vec::new();
        }

        let mut hits: Vec<Hit<'_>> = Vec::new();

        if !bare.contains("::") {
            // **One word is about the name.** The names are kept sorted by their lowercase form, which is the order
            // the answer is reported in, so the search opens them best rank first and *stops when it has enough*:
            //
            // ```text
            // exact      the one key equal to the query
            // prefix     the keys from the query on, while they begin with it        (a range, not a scan)
            // substring  the remaining keys that contain it                          (a scan that stops early)
            // ```
            if let Some(spellings) = self.names.spellings_of(bare) {
                self.open_group(spellings, Some(0), bare, limit, &mut hits);
            }

            if hits.len() < limit {
                for (key, spellings) in self.names.lowered_from(bare) {
                    if !key.starts_with(bare) {
                        break;
                    }
                    if key == bare {
                        continue;
                    }
                    self.open_group(spellings, Some(1), bare, limit, &mut hits);
                    if hits.len() >= limit {
                        break;
                    }
                }
            }

            if hits.len() < limit {
                for (key, spellings) in self.names.lowered_from("") {
                    if key.starts_with(bare) || !key.contains(bare) {
                        continue;
                    }
                    self.open_group(spellings, Some(2), bare, limit, &mut hits);
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
        } else {
            // **A qualified query is about the last segment's prefix**, so only the names that begin with it are
            // opened — in name order, and the rank (exact last segment, or a prefix of it) is decided per fact by
            // the tail of its qualified name.
            let last_asked = bare.rsplit("::").next().unwrap_or_default();

            for (key, spellings) in self.names.lowered_from(last_asked) {
                if !key.starts_with(last_asked) {
                    break;
                }
                self.open_group(spellings, None, bare, limit, &mut hits);
                if hits.len() >= limit {
                    break;
                }
            }

            // A fact with no name has its scope's last segment for one, so it is not where the name order looks.
            // There are few of them, and the rank decides; whatever the ordered walk left out is later than the
            // `limit` it already has, so adding these to it cannot push out an answer that belongs.
            if !last_asked.is_empty() {
                self.open_group(&[Box::from("")], None, bare, limit, &mut hits);
            }
        }

        sort_hits(&mut hits);
        hits.into_iter()
            .take(limit)
            .map(|hit| ProjectSymbol {
                file: hit.file.to_path_buf(),
                fact: hit.fact.clone(),
            })
            .collect()
    }

    /// Open every spelling of one lowercase name: their hits, in the order [`sort_hits`] defines.
    ///
    /// **A name declared more than [`POPULAR_NAME`] times is cut at `limit` hits, in file order.** `size`, `begin` and
    /// `run` are declared once per class that has one, and ordering *all* of them by qualified name to show the first
    /// hundred would cost a project's worth of work for an answer no reader can tell from any other hundred. The cut
    /// is deterministic (postings are in insertion order), and a name below the threshold is ordered exactly.
    fn open_group<'a>(
        &'a self,
        spellings: &[Box<str>],
        rank: Option<usize>,
        wanted: &str,
        limit: usize,
        into: &mut Vec<Hit<'a>>,
    ) {
        let start = into.len();
        for spelling in spellings {
            let postings = self.names.named(spelling);
            let stop_at = match postings.len() > POPULAR_NAME {
                true => start + limit,
                false => usize::MAX,
            };
            self.hits_in(postings, rank, wanted, stop_at, into);
        }
        into[start..].sort_by(hit_order);
    }

    /// The hits among one name's postings, with the cooked reading deduplicated against the raw one.
    ///
    /// `rank` is the rank the caller already knows (a whole bucket of names shares one), or `None` when the rank has
    /// to be worked out from the declaration's qualified name.
    fn hits_in<'a>(
        &'a self,
        postings: &[Posting],
        rank: Option<usize>,
        wanted: &str,
        stop_at: usize,
        into: &mut Vec<Hit<'a>>,
    ) {
        for (at, posting) in postings.iter().enumerate() {
            if into.len() >= stop_at {
                break;
            }
            let Some((summary, fact)) = self.resolve(*posting) else {
                continue;
            };

            // A fact both readings found is one symbol: the raw reading's postings for this file come first in the
            // list, and the same `(name, kind)` among them shadows the cooked one.
            if posting.is_cooked() {
                let shadowed = postings[..at]
                    .iter()
                    .rev()
                    .take_while(|held| held.file == posting.file)
                    .filter(|held| !held.is_cooked())
                    .filter_map(|held| self.resolve(*held))
                    .any(|(_, raw)| raw.kind == fact.kind);
                if shadowed {
                    continue;
                }
            }

            let qualified = fact.qualified_name();
            let Some(rank) = rank.or_else(|| rank_of(&qualified, &fact.name, wanted)) else {
                continue;
            };

            let qualified = qualified.to_lowercase();
            // What the fact is called for ordering: its name, or — with none — the last segment of its scope.
            let name = match fact.name.is_empty() {
                false => crate::index::names::lowercase(&fact.name),
                true => qualified.rsplit("::").next().unwrap_or_default().to_string(),
            };

            into.push(Hit {
                rank,
                name,
                qualified,
                file: &summary.path,
                posting: *posting,
                fact,
            });
        }
    }

    /// The fact a posting points at, and the summary of the file it is in.
    fn resolve(&self, posting: Posting) -> Option<(&FileSummary, &DeclFact)> {
        let key = self.order.get(&posting.file)?;
        let summary = self.summaries.get(key)?;
        let fact = if posting.is_cooked() {
            self.cooked.get(key)?.declarations.get(posting.index())?
        } else {
            summary.declarations.get(posting.index())?
        };
        Some((summary, fact))
    }

    /// The files that **define** the macro `name`, in insertion order — one lookup, where the question used to be a
    /// scan of every file's macro facts.
    pub fn files_defining_macro(&self, name: &str) -> Vec<PathBuf> {
        self.names
            .definers_of(name)
            .iter()
            .filter_map(|sequence| self.order.get(sequence))
            .filter_map(|key| self.summaries.get(key))
            .map(|summary| summary.path.clone())
            .collect()
    }

    /// The summaries of the files that define **any** of these macros, in insertion order.
    ///
    /// The set form of [`ProjectIndex::files_defining_macro`], for a caller that holds a whole vocabulary — the
    /// words a file mentions — and wants the files that could matter to it, without visiting the ones that cannot.
    pub fn files_defining_any_macro<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Vec<&FileSummary> {
        let mut sequences: BTreeSet<u32> = BTreeSet::new();
        for name in names {
            sequences.extend(self.names.definers_of(name).iter().copied());
        }

        sequences
            .into_iter()
            .filter_map(|sequence| self.order.get(&sequence))
            .filter_map(|key| self.summaries.get(key))
            .collect()
    }

    /// How many distinct declared names the index holds.
    pub fn distinct_names(&self) -> usize {
        self.names.distinct_names()
    }

    /// The files in which `name` is visible, in insertion order.
    ///
    /// `name` is matched against a declaration's **qualified** name first — `ns::Widget` — and against its bare
    /// name only as a fallback, because a qualified spelling is a much stronger claim than a name that happens to
    /// appear in some scope. A caller that gets one answer from the qualified match should prefer it to any
    /// number from the bare one.
    pub fn files_declaring(&self, name: &str, visible_from: &Path) -> Vec<VisibleDeclaration<'_>> {
        // The name is looked up under the two spellings a fact can answer to: the whole thing (a bare name) and its
        // last segment (`ns::Widget` is `Widget` declared in `ns`). `matches` then decides between them.
        let bare = name.strip_prefix("::").unwrap_or(name);
        let last = bare.rsplit("::").next().unwrap_or(bare);
        self.visible_declarations(visible_from, |fact| matches(fact, name), Narrow::Named(&[bare, last]))
    }

    /// Every declaration written **directly in** the scope `scope`, visible from `visible_from`.
    ///
    /// The whole-scope counterpart of [`ProjectIndex::definition`]: that asks about one name, this asks about
    /// every name one scope holds, which is what a member list is made of. `scope` is a **qualified** spelling —
    /// `Widget`, `ns::Widget` — because that is what a [`DeclFact`] records; see [`DeclFact::scope`].
    ///
    /// Note what "directly in" excludes, because it is the whole reason this is not a name search: a local
    /// variable inside a member function is a fact whose scope is `None` — a function body contributes no segment
    /// to a qualified name — so `C`'s members are `C`'s bindings and not everything written between its braces.
    pub fn declarations_in(&self, scope: &str, visible_from: &Path) -> Vec<VisibleDeclaration<'_>> {
        self.visible_declarations(
            visible_from,
            |fact| fact.scope.as_deref() == Some(scope),
            Narrow::Scoped(scope),
        )
    }

    /// [`ProjectIndex::declarations_in`] with a **second** condition, applied while the facts are still borrowed.
    ///
    /// The second predicate exists for one caller and one reason: a completion has a prefix, and on a file that
    /// includes `<string>` the scope it is listing holds over a thousand declarations. A caller that collected them
    /// and then kept the ones beginning with `w` would do a `DeclFact` clone, a provenance lookup and a `Vec` push
    /// per name to throw 99% of them away — on every keystroke. The two conditions are asked together because
    /// there is no useful notion of order between them: both are filters, and a fact that fails either is not
    /// collected.
    ///
    /// A method rather than a parameter on [`ProjectIndex::declarations_in`] because that one's callers do not have
    /// a second condition, and a query with an optional filter it never uses is a query whose filter is untested.
    pub fn declarations_in_where(
        &self,
        scope: &str,
        visible_from: &Path,
        accepts: impl Fn(&DeclFact) -> bool,
    ) -> Vec<VisibleDeclaration<'_>> {
        self.visible_declarations_upto(
            visible_from,
            &|fact: &DeclFact| fact.scope.as_deref() == Some(scope) && accepts(fact),
            MAX_COLLECTED_NAMES,
            Narrow::Scoped(scope),
        )
    }

    /// The visibility walk over the declaration list, with the door open for a caller outside this module and
    /// capped at `MAX_COLLECTED_NAMES`.
    ///
    /// The cap and not the whole set, because the one caller is a completion and the names this drops could not
    /// have reached the top of its list — see `MAX_COLLECTED_NAMES`, which states the argument. A caller that wants
    /// **every** visible declaration asks [`ProjectIndex::files_declaring`] or [`ProjectIndex::declarations_in`],
    /// which have no cap because their answers are about one name rather than a list.
    pub fn visible_declarations_where(
        &self,
        visible_from: &Path,
        accepts: impl Fn(&DeclFact) -> bool,
    ) -> Vec<VisibleDeclaration<'_>> {
        self.visible_declarations_upto(visible_from, &accepts, MAX_COLLECTED_NAMES, Narrow::Nothing)
    }

    /// The declarations some predicate accepts, in the files `visible_from` can see.
    ///
    /// The one place the visibility walk is applied to the declaration list, so that a new query over facts
    /// cannot forget it and quietly answer with a declaration in a file the querying file does not include —
    /// which is a jump to something it cannot compile against. `includers_of` and [`ProjectIndex::visible_files`]
    /// are the graph; this is the graph applied to a question.
    ///
    /// # Two readings, one candidate list
    ///
    /// Each visible file contributes its **raw** declarations and, when it has been cooked, its **cooked** ones
    /// (see the `cooked` field). They are unioned rather than chosen between, and the reason is that the query
    /// already carries the discriminator: a caller asking for `std::to_string` cannot be answered by the raw
    /// reading's file-scope `to_string`, and one asking for `to_string` gets both — which is honestly *ambiguous*,
    /// because which one it means depends on the scope the caller is in. What the union must not do is count the
    /// same declaration twice: a declaration both readings found (`(qualified name, kind)`) is one candidate, or
    /// every ordinary declaration would come back as two and every lookup would answer `Ambiguous`.
    ///
    /// # Why the graph is walked once, and not once per file
    ///
    /// This used to ask `visibility_of` for **each summary** in the index, and each of those answers walked the
    /// include graph from scratch: a query over a standard-library closure therefore did three hundred
    /// breadth-first searches to answer one question. Measured, on a closure of 308 files: **573 ms** for a
    /// completion at `std::`, which is a query a client asks on every keystroke. One walk and a map lookup is the
    /// same answer in under a millisecond.
    fn visible_declarations<'a>(
        &'a self,
        visible_from: &Path,
        accepts: impl Fn(&DeclFact) -> bool,
        narrow: Narrow<'_>,
    ) -> Vec<VisibleDeclaration<'a>> {
        self.visible_declarations_upto(visible_from, &accepts, usize::MAX, narrow)
    }

    /// [`ProjectIndex::visible_declarations`] with a **cap** on how many are collected.
    ///
    /// The cap is what makes a completion over a standard-library closure cheap, and it is safe for the same
    /// reason the collection is: a caller that wants at most `n` names and ranks them by **how near the cursor
    /// their declaration is** cannot be affected by the `n + 1`-th, because every name this walk produces is from
    /// a file reached through an include — the tier that ranks last. See [`MAX_COLLECTED_NAMES`].
    ///
    /// `usize::MAX` is the honest spelling of "no cap" rather than an `Option`, because the two callers that pass
    /// it are asking a question about a *name* rather than a list — a jump, a rename — and their answers are
    /// allowed to be as many as there are.
    fn visible_declarations_upto<'a>(
        &'a self,
        visible_from: &Path,
        accepts: &impl Fn(&DeclFact) -> bool,
        limit: usize,
        narrow: Narrow<'_>,
    ) -> Vec<VisibleDeclaration<'a>> {
        // **Only the files the cursor's file can see are looked at**, and in insertion order. The walk answers with
        // a handful of hundreds even when the project holds a hundred thousand files, so the cost of the question is
        // the size of the closure and not the size of the project.
        let files = self.visible_in_order(visible_from);

        let mut found = Vec::new();

        // **A name or a scope narrows further**: the postings say which files hold a candidate at all, so a file
        // that has none is never opened. Without a narrowing every visible file's declarations are the candidates.
        let postings: Option<std::borrow::Cow<'_, [Posting]>> = match narrow {
            Narrow::Nothing => None,
            Narrow::Scoped(scope) => Some(std::borrow::Cow::Borrowed(self.names.scoped(scope))),
            Narrow::Named(names) => {
                let mut keys: Vec<&str> = names.to_vec();
                keys.dedup();
                match keys.as_slice() {
                    [one] => Some(std::borrow::Cow::Borrowed(self.names.named(one))),
                    _ => {
                        let mut merged: Vec<Posting> = keys
                            .iter()
                            .flat_map(|key| self.names.named(key).iter().copied())
                            .collect();
                        merged.sort_unstable();
                        merged.dedup();
                        Some(std::borrow::Cow::Owned(merged))
                    }
                }
            }
        };

        // One `(file, its candidates)` group at a time, in file order. Without postings a file's candidates are all of
        // its facts, which is what the empty group range stands for.
        let mut next = 0;
        let mut visible = files.iter();

        loop {
            // **The cap is checked before a file is read, not while it is.** A file is one declaration list, and
            // the only thing worth stopping on is a whole one: taking half of a file's facts would leave the
            // cooked-vs-raw deduplication below comparing against a `raw` list that is itself half a file, and
            // `(name, kind)` identity is per file. So a query takes files until it has enough, and "enough" is a
            // number no answer can reach past — see [`MAX_COLLECTED_NAMES`].
            if found.len() >= limit {
                break;
            }

            let (file, visibility, group): (u32, IncludeVisibility, Option<&[Posting]>) = match &postings {
                None => {
                    let Some(&(file, visibility)) = visible.next() else {
                        break;
                    };
                    (file, visibility, None)
                }
                Some(postings) => {
                    let Some(first) = postings.get(next) else {
                        break;
                    };
                    let file = first.file;
                    let end = next + postings[next..].partition_point(|held| held.file == file);
                    let group = &postings[next..end];
                    next = end;

                    let Ok(at) = files.binary_search_by_key(&file, |visible| visible.0) else {
                        continue;
                    };
                    (file, files[at].1, Some(group))
                }
            };

            let Some(key) = self.order.get(&file) else {
                continue;
            };
            let Some(summary) = self.summaries.get(key) else {
                continue;
            };

            // The cooked reading is looked up **once**, because a file that was never cooked — which is most of
            // them — has nothing to union and must not pay for the `raw` list below.
            let cooked = self.cooked.get(key);

            let mut raw: Vec<&DeclFact> = Vec::new();

            match group {
                None => raw.extend(summary.declarations.iter().filter(|fact| accepts(fact))),
                Some(group) => raw.extend(
                    group
                        .iter()
                        .filter(|posting| !posting.is_cooked())
                        .filter_map(|posting| summary.declarations.get(posting.index()))
                        .filter(|fact| accepts(fact)),
                ),
            }

            for fact in &raw {
                found.push(VisibleDeclaration {
                    file: summary.path.clone(),
                    fact,
                    visibility,
                });
            }

            let Some(cooked) = cooked else {
                continue;
            };

            // …and what the file was **cooked** into, minus what the raw reading already said. `(name, kind)` is
            // the identity a candidate is deduplicated by — see the method's note.
            let candidates: Box<dyn Iterator<Item = &DeclFact> + '_> = match group {
                None => Box::new(cooked.declarations.iter()),
                Some(group) => Box::new(
                    group
                        .iter()
                        .filter(|posting| posting.is_cooked())
                        .filter_map(|posting| cooked.declarations.get(posting.index())),
                ),
            };

            for fact in candidates.filter(|fact| accepts(fact)) {
                if raw.iter().any(|known| known.name == fact.name && known.kind == fact.kind) {
                    continue;
                }
                found.push(VisibleDeclaration {
                    file: summary.path.clone(),
                    fact,
                    visibility,
                });
            }
        }

        found
    }

    /// The files `from` can see, as `(sequence number, how)` in insertion order — the walk's answer put in the order
    /// every declaration query reports in.
    fn visible_in_order(&self, from: &Path) -> Vec<(u32, IncludeVisibility)> {
        let mut files: Vec<(u32, IncludeVisibility)> = self
            .visible_files(from)
            .into_iter()
            .filter_map(|(key, visibility)| Some((*self.sequence.get(&key)?, visibility)))
            .collect();

        files.sort_unstable_by_key(|(sequence, _)| *sequence);
        files.dedup_by_key(|(sequence, _)| *sequence);
        files
    }

    /// Every file `from` can see, and how — one walk of the include graph.
    ///
    /// The whole-graph answer to what used to be a per-file question: "can this file see that one" was asked once
    /// per summary, and each answer walked the include graph. This returns the answer for every file at once, so
    /// one walk serves a whole query — measured on a closure of 308 files, that turned a 573 ms completion into a
    /// 3 ms one, and every cross-file query over declarations goes through it.
    ///
    /// **Every file that can see `path`** — its transitive includers, nearest first.
    ///
    /// The reverse of [`ProjectIndex::visible_files`], and the question a *change* to a file asks: whose reading was
    /// built while this file said what it used to say? A cooked reading is a reading of its environment as well as of
    /// its text, so a file that defines a macro is read differently by everything that includes it, at any depth.
    ///
    /// Breadth-first over the reverse edges, deduplicated and bounded by the same depth the forward walk uses: an
    /// include graph is not a tree — every header guard makes it a diamond — so "included by, transitively, without
    /// asking about a file twice" is the whole job. The order is the walk's, which is deterministic because the
    /// reverse edges are a `BTreeSet`.
    ///
    /// The **file itself is not in the answer**: a caller dropping that file's own reading has its own reason to, and
    /// one list that means two things is how a caller ends up dropping something twice or not at all.
    pub fn dependents_of(&self, path: &Path) -> Vec<PathBuf> {
        let start = normalize(path);

        let mut seen: HashSet<String> = HashSet::from([start.clone()]);
        let mut pending: VecDeque<(String, usize)> = VecDeque::from([(start, 0)]);
        let mut dependents = Vec::new();

        while let Some((current, depth)) = pending.pop_front() {
            if depth >= MAX_VISIBILITY_DEPTH {
                continue;
            }

            let Some(includers) = self.included_by.get(&current) else {
                continue;
            };

            for includer in includers {
                if !seen.insert(includer.clone()) {
                    continue;
                }
                dependents.push(PathBuf::from(includer));
                pending.push_back((includer.clone(), depth + 1));
            }
        }

        dependents
    }

    /// The visibility recorded is the **best** one found: a file reached both unconditionally and through a
    /// guarded `#include` is unconditional, because the unconditional path is the one that is always there. That
    /// is why a file may be relaxed rather than only visited — the first path found is not necessarily the best,
    /// and a graph with a diamond in it (which every header guard produces) has exactly that shape.
    pub fn visible_files(&self, from: &Path) -> Vec<(String, IncludeVisibility)> {
        let from = normalize(from);

        // The file itself is visible to itself, and unconditionally: a question asked in a file is about what
        // that file says before anything it includes.
        let mut best: HashMap<String, IncludeVisibility> =
            HashMap::from([(from.clone(), IncludeVisibility::Unconditional)]);
        let mut order: Vec<String> = vec![from.clone()];
        let mut pending: Vec<(String, IncludeVisibility, usize)> =
            vec![(from, IncludeVisibility::Unconditional, 0)];

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

                // **A guarded `#include` stays `Conditional` here, and the reason is now cost**.
                //
                // The conditions *are* answerable — `crate::index::environment::visibility_at` answers
                // `#if _STL_COMPILER_PREPROCESSOR` correctly once its two upstream bugs are fixed (`own_guard` not
                // recognising `#pragma once` + `#ifndef`, and a fact in an `#else` judged by the `#if`'s verdict) —
                // and wiring it in here is what finally resolved MSVC's `std::string`: the probe went 2/9 → 7/9.
                //
                // What it also did is make the pinned probe run for **minutes**: `visible_files` walks the whole
                // include graph, and asking the evaluator per guarded include builds a closure state per include
                // (`macros_at`). The libstdc++ closure has hundreds of them. So the rule is right and the
                // *implementation* is not: it needs the incremental state `ProjectIndex::macro_environment` builds
                // (one walk per file, conditions answered as they are reached) before it can be used here — and
                // until then this line stays, because a query that takes minutes is not an answer.
                let step = match include.guard {
                    FactGuard::Unconditional => so_far,
                    // **The condition is asked, and the answer is remembered**. Asking costs the file's
                    // whole closure state; the same question is asked by every query and every walk over hundreds of
                    // edges, and the answer cannot change until a summary is inserted — so it is memoised on the
                    // index (`visibility_answers`), which `insert_at` clears.
                    //
                    // Only `Active` is *used*: a condition that holds makes the include unconditional, and one
                    // nobody can decide leaves the answer exactly where it was (`Conditional`). What `Inactive`
                    // does — dropping the edge — is decided below, where the visibility is turned into a step.
                    FactGuard::Region(region) => {
                        let key = (current.clone(), region);
                        let answer = match self
                            .visibility_answers
                            .lock()
                            .ok()
                            .and_then(|answers| answers.get(&key).copied())
                        {
                            Some(answer) => answer,
                            None => {
                                let answer = crate::index::environment::visibility_at(
                                    self,
                                    Path::new(&current),
                                    include.guard,
                                    include.range.start_offset,
                                );
                                if let Ok(mut answers) = self.visibility_answers.lock() {
                                    answers.insert(key, answer);
                                }
                                answer
                            }
                        };

                        match answer {
                            crate::Visibility::Active => so_far,
                            // **A condition that was decided, and not taken**: the file is not part of this
                            // translation unit at all, so nothing in it is visible — the edge is dropped rather
                            // than downgraded, and so is everything only reachable through it.
                            //
                            // This rule was written twice and reverted twice, and the comment that stood here said
                            // why: the *evaluator* was wrong on two corpora (an unrecognised own guard, a fact in an
                            // `#else`), and a rule that can only add facts is the safe direction while that is true.
                            // Both bugs are fixed, so it was re-measured, and the number it is worth is large:
                            // measured on one real file, `#include <arm_neon.h>` and `#include <zmmintrin.h>` —
                            // ARM and AVX-512 intrinsics, gated on `_M_ARM64`/`_M_AVX512`, both decidable and both
                            // **false** for this compilation — were in the visibility list as `Conditional`, and
                            // their names were **8 of every 10 completion items**: 14351 items, 4.5 MB of JSON, for
                            // a cursor in a 78-line file (`zmmintrin.h` 5256, `arm64_neon.h` 2852, `arm_neon.h` 2117).
                            crate::Visibility::Inactive => continue,
                            // A condition nobody can decide: the file may or may not be there, which is exactly what
                            // `Conditional` means to every consumer.
                            crate::Visibility::Unknown => IncludeVisibility::Conditional,
                        }
                    }
                };
                match best.get(&next) {
                    // Already known, and no worse: nothing new to explore through it.
                    Some(known) if *known <= step => continue,
                    Some(_) => {}
                    None => order.push(next.clone()),
                }

                best.insert(next.clone(), step);
                pending.push((next, step, depth + 1));
            }
        }

        order
            .into_iter()
            .filter_map(|path| best.get(&path).map(|visibility| (path.clone(), *visibility)))
            .collect()
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
    ///
    /// # It is the *one* projection of a plural answer
    ///
    /// [`ProjectIndex::definitions`] is the query; this is the same answer restricted to the names that have
    /// exactly one declaration. A consumer that can show a list — `textDocument/definition` is one — should ask
    /// that instead, because `Ambiguous` is a dead end there: measured on one real file, **46** of its identifiers
    /// answered `Ambiguous`, and every one of them had a perfectly good list behind it (an overload set, a
    /// redeclaration, a namespace).
    pub fn definition(&self, name: &str, visible_from: &Path) -> Known<ProjectDefinition> {
        match self.definitions(name, visible_from) {
            Known::Yes(mut found) if found.found.len() == 1 => Known::Yes(found.found.remove(0)),
            Known::Yes(_) => Known::Unknown(UnknownReason::Ambiguous(Box::from(name))),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::No,
        }
    }

    /// **Every declaration a name refers to** — the plural answer, for a consumer that can show a list.
    ///
    /// # Why a list, and why it is not a preference
    ///
    /// One name covering several entities is ordinary C++, and the language says what they are: `find` in
    /// `std::basic_string` is one **overload set** (seventeen declarations in MSVC's library, every one of them an
    /// answer to "where is this declared"), `std::char_traits` is a class template and its specializations, and
    /// `cin` is written twice in `<iostream>` — one variable, declared twice, which the language calls the same
    /// entity. `Ambiguous` is the honest answer to *which one*, and it is a dead end for a client; the protocol
    /// has a shape for the honest answer, so this is it.
    ///
    /// # The one case that is collapsed, and why that is not taste
    ///
    /// When **every** candidate is a namespace, there is exactly one entity: a namespace is reopened by every file
    /// that writes `namespace std { … }`, and the language says all of those are the same namespace. Measured on
    /// the file that motivated this query: `std` has **58** declarations in the index, so a peek list of
    /// fifty-eight entries answers a question nobody asked — while `size`'s seventeen and `cin`'s two are exactly
    /// what a reader wants to see. The collapse is therefore about the *kind* of entity, not about the count.
    ///
    /// # The order, which is part of the answer
    ///
    /// Unconditional first, then by file and offset. Imposed rather than inherited: `summaries()` is a map, so
    /// leaving the order alone would make the list depend on hashing, and a client's peek list would reorder
    /// itself between two identical requests.
    pub fn definitions(&self, name: &str, visible_from: &Path) -> Known<ProjectDefinitions> {
        let (mut certain, conditional) = match self.certain_declarations(name, visible_from) {
            Ok(candidates) => candidates,
            Err(reason) => return Known::Unknown(reason),
        };

        // See the note above: one namespace, however many declarations spell it.
        if certain
            .iter()
            .all(|found| found.fact.kind == DeclKind::Namespace)
        {
            certain.truncate(1);
        }

        Known::Yes(ProjectDefinitions {
            found: certain
                .into_iter()
                .map(|found| ProjectDefinition {
                    file: found.file,
                    fact: found.fact,
                })
                .collect(),
            conditional,
        })
    }

    /// **What kind of thing a name is, when every declaration the index holds for it agrees** — and
    /// [`UnknownReason::Ambiguous`] when they do not.
    ///
    /// The question a semantic highlighter asks about a name it cannot resolve to one declaration: it has to draw a
    /// colour, and a colour is a claim about the *kind*. An overload set is one kind (`pick(int)` and `pick(double)`
    /// are both functions, and a reader who sees both drawn as functions has been told the truth about both), while
    /// `size` as a member of one class and a free function somewhere else is genuinely two answers, and a client
    /// shown either one of them has been told something false about the other.
    ///
    /// This is not [`ProjectIndex::definition`] with a looser rule — it is the same candidate set, read for the two
    /// fields a highlighter needs instead of cloned whole into an answer a jump would use. See
    /// [`crate::semantic::classified_names`], which is its only caller.
    pub fn kind_of(&self, name: &str, visible_from: &Path) -> Known<IndexedKind> {
        let (certain, _) = match self.certain_declarations(name, visible_from) {
            Ok(candidates) => candidates,
            Err(reason) => return Known::Unknown(reason),
        };

        let mut kinds: Vec<IndexedKind> = certain
            .into_iter()
            .map(|found| IndexedKind {
                kind: found.fact.kind,
                scope: found.fact.scope.map(Box::from),
            })
            .collect();
        kinds.sort_unstable();
        kinds.dedup();

        match kinds.as_slice() {
            [only] => Known::Yes(only.clone()),
            [] => Known::Unknown(UnknownReason::ConditionalCompilation),
            _ => Known::Unknown(UnknownReason::Ambiguous(Box::from(name))),
        }
    }

    /// The declarations the index holds for `name` and `visible_from` can see: the ones that are certainly in scope,
    /// in the order every declaration answer reports in, plus a **count** of the ones reachable only through a
    /// conditional `#include`.
    ///
    /// The walk [`ProjectIndex::definitions`] and [`ProjectIndex::kind_of`] share. It is a function rather than two
    /// copies because the rule it applies — a declaration the file writes itself wins over one it includes, ordered
    /// by file then offset, with the conditional ones counted rather than offered — is the part of a definition
    /// answer that is easy to get subtly different in two places.
    fn certain_declarations(
        &self,
        name: &str,
        visible_from: &Path,
    ) -> Result<(Vec<ProjectDeclaration>, usize), UnknownReason> {
        let mut candidates = self.files_declaring(name, visible_from);

        if candidates.is_empty() {
            return Err(UnknownReason::NotDeclaredHere(Box::from(name)));
        }

        // A declaration the file itself writes wins over one it includes, and is the one a reader means: a
        // header's `Widget` and this file's `Widget` are different entities, and C++ resolves to the local one.
        // Overloads of one name written *here* are still several declarations, and they are all answers.
        let own = normalize(visible_from);
        if candidates
            .iter()
            .any(|found| normalize(&found.file) == own)
        {
            candidates.retain(|found| normalize(&found.file) == own);
        }

        // What is reachable only through a conditional `#include` is counted rather than offered: whether it is in
        // scope depends on macros this layer does not have, and a jump to a declaration that may not be there is a
        // wrong answer rather than a missing one. The count is what keeps the answer from claiming there is
        // nothing else — "one declaration, and two that might be" is not "one declaration".
        let conditional = candidates
            .iter()
            .filter(|found| found.visibility == IncludeVisibility::Conditional)
            .count();

        let mut certain: Vec<ProjectDeclaration> = candidates
            .into_iter()
            .filter(|found| found.visibility == IncludeVisibility::Unconditional)
            .map(|found| ProjectDeclaration::of(&found))
            .collect();

        if certain.is_empty() {
            return Err(UnknownReason::ConditionalCompilation);
        }

        certain.sort_by(|one, other| {
            one.file
                .cmp(&other.file)
                .then_with(|| one.start_offset.cmp(&other.start_offset))
        });

        Ok((certain, conditional))
    }

    /// What the macro name `name` is at `offset` in the file at `visible_from`.
    ///
    /// # Translation order is the whole answer
    ///
    /// A preprocessor reads a translation unit as one stream: the file's own lines, with each `#include` replaced
    /// by the whole of the file it names — recursively — and the macros in force at a point are decided by the
    /// **last** `#define` or `#undef` of that name to have gone past. So the position of a fact is not an offset
    /// but a **chain** of them: the `#include` that pulled its file in, then the `#include` inside *that* file, and
    /// so on, ending with the fact's own offset. Comparing two chains lexicographically *is* comparing where they
    /// came in the stream, which is why this needs no separate rules for "written here" and "written in a header":
    ///
    /// ```text
    /// a.cpp:   #define MAX 1        position [0]         -> the header's wins
    /// a.cpp:   #include "x.h"       position [10, 4]
    /// a.cpp:   #include "x.h"                          -> the local one wins
    /// a.cpp:   #define MAX 1         position [0]
    /// ```
    ///
    /// # The four answers
    ///
    /// * `Yes` — the last fact in that order is a `#define`.
    /// * `Unknown(UndefinedHere)` — it is an `#undef`. Not `No`, and not a pointer at the `#define` it used to
    ///   have: a name that was undefined above the cursor is an ordinary identifier, and a jump to a definition
    ///   that is no longer in force would be a wrong answer rather than a missing one.
    /// * `Unknown(ConditionalCompilation)` — every fact that could decide it is inside an `#if`, or reached
    ///   through an `#include` that is. See below for the rule that keeps this from swallowing the common case.
    /// * `Unknown(NotDeclaredHere)` — nothing in the index touches the name. **Not** "there is no such macro":
    ///   the index holds the files it has been asked about, and a macro defined in a header nobody indexed looks
    ///   exactly like a name that is not a macro at all.
    ///
    /// # Why the answer prefers the unconditional fact
    ///
    /// The same rule [`ProjectIndex::definition`] uses, for the same reason. An unconditional `#define` above a
    /// *guarded* one is what the name certainly is; reporting `ConditionalCompilation` instead would refuse to
    /// answer a question that has an answer. Only when nothing unconditional is in the running does the answer
    /// become `Unknown` — and the residual uncertainty is stated rather than hidden: a guarded `#undef` *after* an
    /// unconditional `#define` is treated as not having happened.
    pub fn macro_definition(&self, name: &str, visible_from: &Path, offset: usize) -> Known<ProjectMacro> {
        match self.macro_environment(name, visible_from).at(offset) {
            Known::Yes(found) => Known::Yes(ProjectMacro {
                file: found.file.clone(),
                fact: found.fact.clone(),
            }),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::No,
        }
    }

    /// Everything this file can know about one macro name, **computed once** and then asked about any offset.
    ///
    /// [`ProjectIndex::macro_definition`] is one walk of the include graph per call, which is the right shape for a
    /// query about one cursor. It is the wrong shape for a query about **thousands** of positions in the same file,
    /// and that is not a hypothetical: `macro_references` asked it once per hit and took **3.8 s** on
    /// `STDMETHODCALLTYPE` in the standard-library closure — 4 079 hits, each walking the same graph, to produce an
    /// answer whose every input was identical. This is that walk, done once per file, with the offset applied to the
    /// result instead of to the search.
    ///
    /// The cost is one traversal of the reachable files per caller, so a caller that asks about one position should
    /// use [`ProjectIndex::macro_definition`] and one that asks about many should hold this.
    pub fn macro_environment(&self, name: &str, visible_from: &Path) -> MacroEnvironment {
        let mut candidates = Vec::new();
        let certain = self.certainly_in_the_translation_unit(visible_from);

        self.macro_candidates(
            visible_from,
            &mut Vec::new(),
            false,
            &mut HashSet::new(),
            &certain,
            Collecting::Name(name, &mut candidates),
            &mut self.macros.clone(),
            None,
        );

        MacroEnvironment {
            name: name.to_string(),
            candidates,
        }
    }

    /// **The macros in force at one point**, as a walk from that file computes them.
    ///
    /// The compilation's own environment (what the compiler predefines and the command line says) plus every fact
    /// the walk passes on the way to `offset`, applied in the order a preprocessor applies them — which is the only
    /// order that is right: a `#define` written below the point is not in force, one written in a header that comes
    /// later is not either, and a header that comes earlier is.
    ///
    /// Two things this is *not*: it is not a claim that the answer is complete (a header outside the index, or a
    /// `-D` nobody mentioned, is simply not in it — see [`crate::index::environment`]), and it is not a summary of
    /// the whole translation unit (the walk stops at `offset`, which is what makes it a question about a point).
    ///
    /// Costs one traversal of the files that precede the point, so a caller asking about **one** position should use
    /// this and a caller asking about thousands of them should use the incremental state
    /// [`ProjectIndex::macro_environment`] builds while it walks.
    pub fn macros_at(&self, path: &Path, offset: usize) -> Marked {
        let mut state = self.macros.clone();
        let certain = self.certainly_in_the_translation_unit(path);

        self.macro_candidates(
            path,
            &mut Vec::new(),
            false,
            &mut HashSet::new(),
            &certain,
            Collecting::Nothing,
            &mut state,
            Some(offset),
        );

        state
    }

    /// The files this query reaches through **unguarded includes only** — no `#if` anywhere on the way.
    ///
    /// This is the answer to "is this file part of the translation unit whatever the macros are", and it is asked
    /// once per walk instead of being inferred from the route the walk happened to take: `visited` enters a
    /// file once, so the first route to reach it decides how everything it defines is filed, and that first route
    /// can be a conditional include while an unguarded one exists elsewhere in the same translation unit.
    ///
    /// # Why not ask `visible_files`
    ///
    /// It answers the same question and answers it *better* — its `Unconditional` also counts an `#include` whose
    /// condition was evaluated and found taken. It cannot be used here: that walk evaluates conditions, evaluating
    /// one calls [`ProjectIndex::macros_at`], and `macros_at` is what builds this set. Written that way it was a
    /// stack overflow on the first run (`macros_at` → `visible_files` → `visibility_at` → `macros_at` → …, with the
    /// memo unable to break the cycle because an answer is only recorded once it has been computed). So what is
    /// taken from that idea is the part that needs **nothing evaluated**, and a guarded include that *is* taken
    /// still counts for the walk through its own route: `visibility == Active` already keeps a route certain, see
    /// `macro_candidates`.
    fn certainly_in_the_translation_unit(&self, from: &Path) -> HashSet<String> {
        let from = normalize(from);

        // The file itself, always: a question asked in a file is about what that file says.
        let mut certain = HashSet::from([from.clone()]);
        let mut pending = vec![from];

        while let Some(current) = pending.pop() {
            let Some(summary) = self.summaries.get(&current) else {
                continue;
            };

            for include in &summary.includes {
                if include.guard != FactGuard::Unconditional {
                    continue;
                }

                let Some(resolved) = &include.resolved else {
                    continue;
                };

                let next = normalize(resolved);
                if certain.insert(next.clone()) {
                    pending.push(next);
                }
            }
        }

        certain
    }

    /// Every fact about `name` in this file and everything it includes, with where each one sits.
    ///
    /// A file is expanded once per query. That is enough for the answer — the facts are the same however many
    /// paths reach them — and it is what makes a cycle of includes terminate. What it costs is the ordering *among
    /// facts reached through the same top-level include*, which is the one case where a header reached twice by
    /// different routes could be placed at the earlier of its two positions rather than the later; the fact it
    /// reports is the same either way.
    ///
    /// No offset is applied here: where a fact *is* in the stream is the fact, and which of them is in force at a
    /// particular cursor is [`MacroEnvironment::at`]'s question. That split is what lets one walk answer thousands
    /// of positions.
    ///
    /// # The stream is one stream
    ///
    /// A file's `#define`s and its `#include`s are read **in offset order**, and the state in `state` is brought up
    /// to each point before anything is asked about that point. That is not an implementation detail: it is what
    /// makes `#ifdef X` decidable in a header whose *includer* defined `X`, and it is why the file's own directives
    /// and the ones its includes bring in are **one** stream rather than two lists. Two lists walked separately
    /// would decide every condition against the environment as it was before the file was read.
    ///
    /// # What the state is allowed to take from a fact
    ///
    /// Only what is **certain**, because the state decides later conditions and a wrong entry would make them
    /// decided wrongly rather than undecided:
    ///
    /// ```text
    /// the file is certainly in the translation unit, and the fact's region is taken → the name, with its value
    /// the fact settles the name whatever the branch                                  → definedness alone, never the value
    /// anything else (the region is unknown, or the file may not be included)         → nothing
    /// ```
    ///
    /// The second line is [`MacroFact::settles_the_name`] doing the same job it does for references: `#ifndef NAME /
    /// #define NAME` says the name *is* a macro afterwards whichever branch ran, and it does **not** say which body
    /// it has — so the name is defined and its value stays unknown.
    ///
    /// The first line used to read "the whole **path** is unconditional", where the path meant the route this walk
    /// took — see `certain`.
    #[allow(clippy::too_many_arguments)]
    fn macro_candidates(
        &self,
        path: &Path,
        chain: &mut Vec<usize>,
        path_conditional: bool,
        visited: &mut HashSet<String>,
        certain: &HashSet<String>,
        mut collecting: Collecting<'_>,
        state: &mut Marked,
        upto: Option<usize>,
    ) {
        let path = normalize(path);
        if !visited.insert(path.clone()) {
            return;
        }

        // **A file this query reaches unconditionally is certainly part of the translation unit**, whatever route
        // this particular walk took to get here. The flag above is about *this* path — `visited` means a file
        // is entered once, so the first route to reach it decided everything below — and the first route can be a
        // conditional include while an unconditional one exists elsewhere in the same translation unit. Asking the
        // graph instead of the route is what lets the certain path be used: `windef.h` includes `winnt.h`
        // unconditionally, so `winnt.h`'s macros are in force, whatever `minwindef.h` did earlier.
        let path_conditional = path_conditional && !certain.contains(&path);

        let Some(summary) = self.summaries.get(&path) else {
            // A file the walk cannot read is a hole in what this state knows: it may define anything, so from here
            // on a name nothing mentions is unknown rather than undefined.
            state.mark_incomplete();
            return;
        };

        // What this walk made of this file's regions, recorded the first time each is asked about. The answer is
        // the one the *condition's own point* gives, and the first fact or include inside a region is the first
        // point at which the walk can ask — before anything inside the region has been applied. Asking again later
        // would read the region's own body as evidence about its condition, which is backwards for exactly the
        // shape that matters: `#ifndef NAME / #define NAME / #include <a.h> / #endif` has to include `a.h` on the
        // first pass, and a fresh look at the same condition after the `#define` would say it does not.
        let mut verdicts: HashMap<u32, Option<bool>> = HashMap::new();

        // The two lists are each in offset order (the indexer sorts them), so one merge of them *is* the order a
        // preprocessor reads in. A fact and an include can never start at the same offset.
        let mut facts = summary.macros.iter().peekable();
        let mut includes = summary.includes.iter().peekable();

        loop {
            let next_fact = facts.peek().map(|fact| fact.range.start_offset);
            let next_include = includes.peek().map(|include| include.range.start_offset);

            let (at, take_fact) = match (next_fact, next_include) {
                (Some(fact), Some(include)) => (fact.min(include), fact <= include),
                (Some(fact), None) => (fact, true),
                (None, Some(include)) => (include, false),
                (None, None) => break,
            };

            if upto.is_some_and(|upto| at >= upto) {
                break;
            }

            if take_fact {
                let fact = facts.next().expect("peeked");
                let reach = self.fact_reach(summary, &mut verdicts, state, fact.guard, at);
                if reach == FactReach::Inactive {
                    continue;
                }

                collecting.fact(summary, chain, fact, path_conditional, reach);

                // …and then the name is in force for everything below this point. After the fact was read for its
                // own guard, which is the order a preprocessor reads in: `#ifndef NAME / #define NAME` is taken
                // *because* the name is not defined yet.
                apply_fact(state, fact, path_conditional, reach);

                continue;
            }

            let include = includes.next().expect("peeked");

            let visibility = self.include_visibility(summary, &mut verdicts, state, include.guard, at);

            if visibility == Visibility::Inactive {
                continue;
            }

            let Some(target) = &include.resolved else {
                // An `#include` that did not resolve is the one gap the walk cannot make smaller: the file it
                // names may define anything at all, so every later "nothing defines this name" answer is off.
                state.mark_incomplete();
                continue;
            };

            chain.push(include.range.start_offset);
            self.macro_candidates(
                target,
                chain,
                path_conditional || visibility == Visibility::Unknown,
                visited,
                certain,
                collecting.reborrow(),
                state,
                // An included file is pasted *at* the include, so all of it is read before the caller's next line.
                None,
            );
            chain.pop();
        }
    }

    /// What the walk makes of one fact's region, recorded per file so that every condition in it is decided the
    /// same way.
    fn fact_reach(
        &self,
        summary: &FileSummary,
        verdicts: &mut HashMap<u32, Option<bool>>,
        state: &Marked,
        guard: FactGuard,
        offset: usize,
    ) -> FactReach {
        match self.include_visibility(summary, verdicts, state, guard, offset) {
            Visibility::Active => FactReach::Active,
            Visibility::Inactive => FactReach::Inactive,
            Visibility::Unknown => FactReach::Unknown,
        }
    }


    /// Was the code the guard names compiled, with the state the walk has built and the verdicts it has recorded?
    fn include_visibility(
        &self,
        summary: &FileSummary,
        verdicts: &mut HashMap<u32, Option<bool>>,
        state: &Marked,
        guard: FactGuard,
        offset: usize,
    ) -> Visibility {
        let FactGuard::Region(region) = guard else {
            return Visibility::Active;
        };

        // **The file's own include guard is not a condition**. `#ifndef _STRING_ / #define _STRING_`
        // followed by the file's includes is how every header is written, and by the time the walk reaches an
        // include the name has been defined *by the line above it* — so evaluating that region says "not taken"
        // and the walk skips **every include of the file**. The visible consequence was MSVC's whole library:
        // `_STL_COMPILER_PREPROCESSOR`, defined by `<yvals_core.h>`, came out `defined = Some(false)` in the state
        // at `<string>`'s own `#include <xstring>`, so `#if _STL_COMPILER_PREPROCESSOR` — a condition that holds —
        // was answered `Inactive`, and every fact behind it read as `Conditional`.
        //
        // The index already treats the facts guarded by exactly this region as unconditional
        // (`deguard_the_files_own_guard`), and `SummaryGuards::own_guard` exists so that a **walk** can apply the
        // same rule — which is what this is. Entering the file at all is what the guard means.
        if summary.guards.own_guard == Some(region) {
            return Visibility::Active;
        }

        let mut unknown = false;

        for at in summary.guards.conditions_of(region) {
            let holds = match verdicts.get(&at.region) {
                Some(recorded) => *recorded,
                None => {
                    let decided = summary
                        .guards
                        .region_at(at.region, offset)
                        .and_then(|region| {
                            region.visibility(&crate::index::environment::MacrosHere::from_walk(
                                state,
                                at.condition_at,
                            ))
                        });

                    verdicts.insert(at.region, decided);
                    decided
                }
            };

            match holds {
                Some(true) => {}
                Some(false) => {
                    // **An `#else` is taken precisely when the condition is *not***. The verdict above
                    // answers "did this region's `#if` hold", which is the right question for the branch the
                    // condition guards and the wrong one for the `#else`, whose whole meaning is "nothing before me
                    // was taken". One of those decides MSVC's entire STL: `yvals_core.h` writes
                    //
                    // ```cpp
                    // #if defined(RC_INVOKED) || defined(Q_MOC_RUN) || defined(__midl)
                    // #define _STL_COMPILER_PREPROCESSOR 0
                    // #else
                    // #define _STL_COMPILER_PREPROCESSOR 1     // ← this is the definition that is in force
                    // #endif
                    // ```
                    //
                    // so the definition that *is* compiled was judged "not taken" (measured: `reach = Inactive`
                    // for both facts), `#if _STL_COMPILER_PREPROCESSOR` then had no value, and every query in the
                    // library answered `ConditionalCompilation`.
                    // Inverted for the #else: see the note above this match.
                    let in_the_else = summary
                        .guards
                        .conditionals
                        .get(at.region as usize)
                        .is_some_and(|conditional| {
                            conditional.branches.iter().any(|branch| {
                                branch.kind == crate::DirectiveKind::Else
                                    && offset >= branch.body.start_offset
                                    && offset <= branch.body.end_offset()
                            })
                        });
                    if in_the_else {
                        continue;
                    }
                    return Visibility::Inactive;
                }
                None => unknown = true,
            }
        }

        if unknown {
            Visibility::Unknown
        } else {
            Visibility::Active
        }
    }
}

/// What a walk is collecting as it goes: the facts about one name, or nothing.
///
/// The same walk answers both questions — "where is this name a macro" ([`ProjectIndex::macro_environment`]) and
/// "what is in force at this point" ([`ProjectIndex::macros_at`]) — because they are the same traversal and the
/// rules that make it right are the ones that must not be written twice. The second caller wants the *state* and
/// no candidates, which is what `Nothing` says, rather than a name no macro has.
enum Collecting<'a> {
    Name(&'a str, &'a mut Vec<MacroCandidate>),
    Nothing,
}

impl Collecting<'_> {
    /// **One** fact, if the caller is collecting facts about that name.
    fn fact(
        &mut self,
        summary: &FileSummary,
        chain: &[usize],
        fact: &MacroFact,
        path_conditional: bool,
        region: FactReach,
    ) {
        let Collecting::Name(name, out) = self else {
            return;
        };

        if fact.name != *name {
            return;
        }

        let mut position = chain.to_vec();
        position.push(fact.range.start_offset);

        out.push(MacroCandidate {
            position,
            file: summary.path.clone(),
            fact: fact.clone(),
            path_conditional,
            region,
        });
    }

    /// The same collection, for a recursive call.
    fn reborrow(&mut self) -> Collecting<'_> {
        match self {
            Collecting::Name(name, out) => Collecting::Name(name, out),
            Collecting::Nothing => Collecting::Nothing,
        }
    }
}

/// What a fact does to the state, if anything — see [`ProjectIndex::macro_candidates`] for the three cases.
///
/// Called *after* the fact's own guard has been read, which is the order a preprocessor reads in: the
/// `#ifndef NAME / #define NAME` shape is taken because the name is not defined yet, and a state that had already
/// absorbed the `#define` would decide it the other way round — for every include guard in the corpus.
fn apply_fact(state: &mut Marked, fact: &MacroFact, path_conditional: bool, reach: FactReach) {
    // A fact in a file that may not have been included at all, or in a region nobody can decide: the name may or
    // may not be defined here, and a later condition must not be told either way.
    if path_conditional || reach == FactReach::Unknown {
        // …unless the region settles the name whatever its own condition says, which is the one thing an
        // undecidable branch *can* still say — and it says it about definedness only, never about the value.
        match (fact.settles_the_name, fact.kind) {
            (true, crate::MacroKind::Definition) => state.define_name(&fact.name),
            (true, crate::MacroKind::Undefinition) => state.undefine(&fact.name),
            _ => state.mark_uncertain(&fact.name),
        }

        return;
    }

    match reach {
        // In force: the name and its value, as the file wrote them.
        FactReach::Active => state.observe(fact),
        // Handled above.
        FactReach::Inactive | FactReach::Unknown => {}
    }
}

/// How reachable a fact is, as the walk sees it: whether its own region is decided.
///
/// A name of its own rather than [`Visibility`], because the walk has a fourth case the guard layer does not: a
/// fact that is *not* written in a conditional at all, which is certain without anything being evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FactReach {
    /// The fact is outside every conditional, or its region is decided and taken.
    Active,
    /// The region is decided and **not** taken: a compiler would not have read this fact.
    Inactive,
    /// At least one enclosing condition cannot be decided here.
    Unknown,
}

/// Every fact about one macro name that a file can reach — the answer to "what is this name here", before the
/// question of *where* is asked.
///
/// Built by [`ProjectIndex::macro_environment`]. The point of holding it is that the expensive half — walking the
/// include graph for the facts — does not depend on the offset, while the cheap half does.
#[derive(Debug, Clone, Default)]
pub struct MacroEnvironment {
    /// The name asked about, kept so that a reason can say *which* name could not be resolved — the walk is over
    /// one name and a caller reading the answer should not have to remember it.
    name: String,
    candidates: Vec<MacroCandidate>,
}

impl MacroEnvironment {
    /// **Which `#define` is in force** at `offset`, or why that cannot be said.
    ///
    /// This is the question "go to macro definition" asks, and it is the strict one. The rules are the
    /// preprocessor's:
    ///
    /// * **only facts pasted in at or before the cursor count.** The first element of a candidate's chain is where
    ///   it enters the file the cursor is in — the fact's own offset, or the `#include` that pulled its file in —
    ///   so one comparison covers both cases: a `#define` written below the use is not in force yet, and neither is
    ///   anything from an `#include` written below it. Everything *inside* an included file counts, because the
    ///   preprocessor pastes all of it at the `#include`.
    /// * **the last one wins**, by lexicographic comparison of those chains.
    /// * **an unconditional fact beats a conditional one** even when the conditional one is later: a fact behind an
    ///   `#if` may not be there at all, so one that is certainly there settles the name — and only when nothing
    ///   certain exists does the conditional one make the answer `Unknown(ConditionalCompilation)`.
    ///
    /// [`MacroFact::settles_the_name`] is deliberately **not** consulted here: it says the name is a macro either
    /// way, not *which* `#define` did it, and in the `#ifndef NAME` shape the definition may have come from an
    /// earlier header this file cannot see. [`MacroEnvironment::is_a_macro_at`] is the question it answers.
    pub fn at(&self, offset: usize) -> Known<&MacroCandidate> {
        let in_force: Vec<&MacroCandidate> = self.in_force(offset).collect();

        let best = last(in_force.iter().copied().filter(|candidate| !candidate.is_conditional()))
            .or_else(|| last(in_force.iter().copied()));

        let Some(best) = best else {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(self.name.as_str())));
        };

        if best.is_conditional() {
            return Known::Unknown(UnknownReason::ConditionalCompilation);
        }

        self.as_answer(best)
    }

    /// **Is this name a macro** at `offset` — the weaker question a find-references asks.
    ///
    /// The difference from [`MacroEnvironment::at`] is one word: a fact inside an `#if` counts here when the
    /// conditional *cannot change whether the name is a macro* afterwards, which is what
    /// [`MacroFact::settles_the_name`] records (the `#ifndef NAME / #define NAME` idiom and a region whose every
    /// branch agrees). That is the whole reason this is a second method rather than a flag on the first: a use is a
    /// use even when which `#define` is in force depends on a macro nobody has.
    ///
    /// The `#undef` case is where the two genuinely part company, so it gets its own rule: a certain `#undef` says
    /// the name is not a macro here — **unless** something that might be in force *after* it is a definition.
    pub fn is_a_macro_at(&self, offset: usize) -> Known<&MacroCandidate> {
        let in_force: Vec<&MacroCandidate> = self.in_force(offset).collect();

        let Some(best) = last(
            in_force
                .iter()
                .copied()
                .filter(|candidate| candidate.is_certain_about_the_name()),
        ) else {
            // Nothing that reaches this offset settles it. Either there is nothing at all — the name is not a
            // macro here — or everything there is depends on a condition.
            return if in_force.is_empty() {
                Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(self.name.as_str())))
            } else {
                Known::Unknown(UnknownReason::ConditionalCompilation)
            };
        };

        if best.is_definition() {
            return Known::Yes(best);
        }

        // The name is certainly *not* a macro here — a `#define` re-asserted inside a conditional is not a second
        // definition, and this fact already answered the question. But a `#undef` can be overturned by a
        // conditional definition that comes later in the stream, and then the honest answer is "maybe".
        let overturned = in_force
            .iter()
            .any(|candidate| candidate.position > best.position && candidate.is_definition());

        if overturned {
            return Known::Unknown(UnknownReason::ConditionalCompilation);
        }

        Known::Unknown(UnknownReason::UndefinedHere(Box::from(self.name.as_str())))
    }

    /// The in-force candidates, in no particular order: everything pasted in at or before `offset`.
    fn in_force(&self, offset: usize) -> impl Iterator<Item = &MacroCandidate> {
        self.candidates
            .iter()
            .filter(move |candidate| candidate.position[0] <= offset)
    }

    /// The answer a settled candidate gives, whether or not its region was settled by the flag.
    fn as_answer<'a>(&self, best: &'a MacroCandidate) -> Known<&'a MacroCandidate> {
        if best.is_definition() {
            return Known::Yes(best);
        }

        Known::Unknown(UnknownReason::UndefinedHere(Box::from(self.name.as_str())))
    }

    /// Is there anything at all to say about the name in this file?
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

/// The last of a run of candidates in translation order.
fn last<'a>(
    candidates: impl Iterator<Item = &'a MacroCandidate>,
) -> Option<&'a MacroCandidate> {
    candidates.max_by(|one, other| one.position.cmp(&other.position))
}

/// One fact about a macro name, and where it sits in the translation unit's stream.
#[derive(Debug, Clone)]
pub struct MacroCandidate {
    /// The chain of offsets that pastes this fact in: the top-level `#include`, each nested one, and the fact's
    /// own offset. Compared lexicographically, which is what makes it a position.
    position: Vec<usize>,
    file: PathBuf,
    fact: MacroFact,
    /// Is one of the `#include`s that pull this fact's file in written inside a conditional?
    ///
    /// Kept apart from the fact's own [`MacroFact::guard`] because the two are answered by different things: a
    /// path through a conditional `#include` means the fact's *file* may not be part of the translation unit at
    /// all, which nothing about the fact's own text can repair.
    path_conditional: bool,
    /// What the environment makes of the fact's own region — see [`MacroCandidate::is_conditional`].
    region: FactReach,
}

impl MacroCandidate {
    /// The file the fact is written in.
    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn fact(&self) -> &MacroFact {
        &self.fact
    }

    /// Is the fact a `#define` (as opposed to an `#undef`)?
    pub fn is_definition(&self) -> bool {
        self.fact.kind.is_definition()
    }

    /// Is **which `#define` is in force** conditional — the question [`MacroEnvironment::at`] answers?
    ///
    /// Yes when the path in is conditional, or when the fact is written inside an `#if` that the environment
    /// cannot decide — including the `#ifndef NAME` whose body defines the name, where the name is certainly a
    /// macro but the *definition* it has may be somebody else's. A region the environment **decides** is not
    /// conditional in this sense: `#ifdef _WIN32` on a machine whose compiler defines `_WIN32` is a fact about
    /// which `#define` is in force, not a doubt about it.
    pub fn is_conditional(&self) -> bool {
        self.path_conditional || self.region == FactReach::Unknown
    }

    /// Is **whether the name is a macro here** settled — the question [`MacroEnvironment::is_a_macro_at`] answers?
    ///
    /// The path in still has to be unconditional: a fact in a header that may not have been included at all says
    /// nothing. The fact's own region is settled when the environment decides it *is* taken, and otherwise by
    /// [`MacroFact::settles_the_name`] — which is exactly the case the two questions differ in.
    pub fn is_certain_about_the_name(&self) -> bool {
        !self.path_conditional
            && (self.fact.settles_the_name || self.region == FactReach::Active)
    }
}

/// A declaration found in another file, with how the file that asked reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleDeclaration<'a> {
    pub file: PathBuf,
    pub fact: &'a DeclFact,
    pub visibility: IncludeVisibility,
}

/// One declaration a name resolved to, **owned** so that the walk that found it can hand it back: a
/// [`VisibleDeclaration`] borrows the index, and the two queries built on this want to sort, dedup and truncate the
/// list before they answer.
struct ProjectDeclaration {
    file: PathBuf,
    fact: DeclFact,
    start_offset: usize,
}

impl ProjectDeclaration {
    fn of(found: &VisibleDeclaration<'_>) -> ProjectDeclaration {
        ProjectDeclaration {
            file: found.file.clone(),
            fact: found.fact.clone(),
            start_offset: found.fact.name_range.start_offset,
        }
    }
}

/// **What the index says a name is**, as the two facts a colour is drawn from — the answer to
/// [`ProjectIndex::kind_of`].
///
/// The pair and not a [`DeclFact`], because a highlighter asks this once per distinct spelling in a file and a
/// `DeclFact` is five owned strings deep: cloning one per spelling to read two fields off it is most of what such a
/// request would cost. See [`ProjectIndex::kind_of`] for why agreement among the candidates is the rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexedKind {
    pub kind: DeclKind,
    /// The qualified scope the declaration was written in, or `None` at file scope.
    ///
    /// Part of the answer rather than a detail: it is what tells a **member** from a free name (`std::string::size`
    /// from `::size`), and it is also why this type is ordered — two candidates are one answer only when both
    /// fields agree.
    pub scope: Option<Box<str>>,
}

/// The answer to a cross-file definition question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDefinition {
    pub file: PathBuf,
    /// The declaration, cloned out of the index so that an answer does not borrow the project for as long as a
    /// consumer wants to hold it — a language server hands the location to a client and moves on.
    pub fact: DeclFact,
}

/// **Every declaration a name refers to** — the plural answer to "where is this declared".
///
/// See [`ProjectIndex::definitions`] for what the list means, why one namespace is collapsed to one entry, and
/// what the order is. This type exists because the answer is genuinely plural: an overload set is several
/// declarations, and a client that can show a list should be given all of them rather than told "ambiguous".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDefinitions {
    /// The declarations to answer with, in the order [`ProjectIndex::definitions`] documents. One entry for the
    /// ordinary case, several for an overload set or a name declared in more than one place.
    ///
    /// Never empty: a name nothing declares is [`UnknownReason::NotDeclaredHere`] rather than an empty list, and a
    /// name whose only declarations are behind a conditional `#include` is
    /// [`UnknownReason::ConditionalCompilation`]. An empty list would be an answer that looks like "here they are"
    /// and says nothing.
    pub found: Vec<ProjectDefinition>,
    /// How many declarations of the same name the index holds that are reachable **only** through a conditional
    /// `#include`, and are therefore not in [`ProjectDefinitions::found`]: whether they are in scope depends on
    /// macros this layer does not have.
    ///
    /// Counted rather than dropped, because "one declaration" and "one declaration and two that might also be
    /// there" are different answers, and a consumer that shows the first without the second is claiming more than
    /// this layer knows.
    pub conditional: usize,
}

impl ProjectDefinitions {
    /// An answer of one declaration — the shape the single-file layer's own resolution produces.
    pub fn one(found: ProjectDefinition) -> Self {
        ProjectDefinitions {
            found: vec![found],
            conditional: 0,
        }
    }

    /// Does this name refer to exactly one declaration?
    pub fn is_one(&self) -> bool {
        self.found.len() == 1
    }
}

/// A type's members, as [`members_of`] lists them.
///
/// # The order is part of the answer
///
/// Members come **nearest class first**: the type's own, then its direct bases', then theirs, level by level.
/// That is the order C++ hides in, so a consumer that shows the list in this order shows the members in
/// precedence order — and a member that a nearer level also declares is not in the list at all, because the
/// language does not find it by that name.
///
/// Within a level the order is by **name**, and it has to be imposed rather than inherited: the file's own scope
/// tree keeps its bindings name-sorted — see [`ScopeTree::add_binding`](crate::ScopeTree::add_binding) — while
/// the index keeps facts in offset order, so leaving each side as it came would make a list depend on whether the
/// class happened to be in the buffer or in a header. Sorting by name is the one rule both sides can obey, and it
/// is stable, so two declarations of one name keep the order they were written in.
///
/// # It is never a claim of completeness
///
/// [`MemberList::unlisted`] names the bases this walk could not open. An empty `unlisted` means "no base this walk
/// reached was left unread" — not "this is everything the compiler would see". The index holds a subset of the
/// translation unit, and the standard library is not in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberList {
    pub members: Vec<ProjectMember>,
    /// The bases whose own members are **not** in this list, with why.
    ///
    /// Separate from `members` rather than folded into it, because the two say different things: `members` is
    /// what the type has, and this is where the answer stops. A consumer that shows a truncated list without
    /// saying so is the failure this field exists to prevent.
    pub unlisted: Vec<UnlistedBase>,
}

impl MemberList {
    /// The members declared by the type itself, as opposed to the ones it inherits.
    pub fn own(&self) -> impl Iterator<Item = &ProjectMember> {
        self.members.iter().filter(|member| member.depth == 0)
    }

    /// The members reached `depth` base steps away: `0` for the type's own, `1` for a direct base's.
    pub fn at_depth(&self, depth: usize) -> impl Iterator<Item = &ProjectMember> {
        self.members.iter().filter(move |member| member.depth == depth)
    }
}

/// One member of a type: where it is declared, and which class in the chain declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMember {
    pub file: PathBuf,
    pub fact: DeclFact,
    /// The **resolved** qualified name of the class that declares it — the type asked about, or one of its bases.
    ///
    /// The qualified name rather than the spelling the derived class wrote, because that is what makes the member
    /// a thing a second query can be asked about: `Base` in `struct D : public Base` is a spelling, and
    /// `ns::Base` is the class. The spelling is in [`DeclFact::bases`] on the derived class's own fact.
    pub declared_in: String,
    /// How many base steps away it is: `0` for the type's own members, `1` for a direct base's, and so on.
    ///
    /// Recorded rather than left to be recomputed from `members`, because a consumer grouping by it — an outline,
    /// a completion that shows inherited members separately — would otherwise have to reconstruct the walk it was
    /// just handed the result of.
    pub depth: usize,
    /// Another class **at the same level** also declares this name, so the name is not uniquely resolved here.
    ///
    /// The list keeps both declarations, because dropping either would be choosing. See [`members_of`].
    pub ambiguous: bool,
}

/// A base whose members are not in the list, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlistedBase {
    /// The base as a **name to look up**, not as the file spelled it: `Base` for `public Base<int>` — a base is
    /// found by name, and the template arguments say which type is inherited rather than which class declares the
    /// members.
    pub spelling: String,
    /// [`UnknownReason::NotDeclaredHere`] for a base nothing visible declares, and
    /// [`UnknownReason::ConditionalCompilation`] for one reachable only through a guarded `#include`.
    pub reason: UnknownReason,
}

impl ProjectDefinition {
    /// The same shape, from a binding the file's own scopes produced.
    ///
    /// [`crate::sema::resolve::definition_at`] answers with a [`Binding`], which carries a `Name` rather than a plain
    /// string and no qualified scope — so the two answers are made to look alike here, in one place, rather than
    /// at every call site.
    ///
    /// `scope` is what the *caller* knows and this does not: the qualified name a member access or a qualifier
    /// went through (`Widget` for `widget.size`, `ns` for `ns::Widget`), or `None` for a name written bare. It is
    /// passed rather than derived because deriving it here would mean re-reading the cursor, and left empty it
    /// would make `widget.size` and a free `size` indistinguishable to a consumer showing the answer.
    ///
    /// `local` is passed for the same reason, one step further out: whether the name can be reached from another
    /// file is a fact about the **scope chain** the binding was made in, and a `Binding` carries a [`crate::ScopeId`]
    /// rather than the tree that id belongs to. The caller has the tree; see [`ScopeTree::declares_a_local`].
    ///
    /// [`Binding`]: crate::Binding
    /// [`ScopeTree::declares_a_local`]: crate::ScopeTree::declares_a_local
    pub fn from_binding(
        path: &Path,
        binding: crate::Binding,
        scope: Option<String>,
        local: bool,
    ) -> Self {
        ProjectDefinition {
            file: path.to_path_buf(),
            fact: DeclFact {
                name: binding
                    .name
                    .identifier_text()
                    .unwrap_or_default()
                    .to_string(),
                scope,
                local,
                kind: crate::DeclKind::from_binding_kind(binding.kind),
                // No type, no return type and no bases, because this answer is a *place to jump to* and the binding
                // it comes from carries none of them: they are facts about the file's text, and the file they
                // belong to has the summary that holds them. A consumer asking what a name *is* asks the index,
                // not this answer.
                type_of: None,
                returns: None,
                bases: Vec::new(),
                range: binding.range,
                name_range: binding.name_range,
                // And no answer about diagnostics either, for the same reason `guard` has none: the binding came
                // from the buffer's own scopes, which are built from a node rather than from a tree with a
                // diagnostic list. See [`fact_from_binding`].
                clean: true,
                guard: FactGuard::Unconditional,
            },
        }
    }
}

/// Is the path to a declaration always taken, or only under conditions the index cannot evaluate?
///
/// Ordered by **how good the answer is**, which is what lets [`ProjectIndex::visible_files`] keep the best path to
/// a file rather than the first one it happens to find: `Unconditional < Conditional`, so "is the known answer no
/// worse than this one" is a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IncludeVisibility {
    /// Every `#include` on the path is outside any `#if`, so the declaration is in scope whatever the macros are.
    Unconditional,
    /// At least one `#include` on the path is inside an `#if` this layer cannot evaluate.
    Conditional,
}

/// What a declaration query knows about the fact it is looking for, before it reads a single fact.
#[derive(Clone, Copy)]
enum Narrow<'q> {
    /// Nothing: every fact of every visible file is a candidate.
    Nothing,
    /// The fact's bare name is one of these.
    Named(&'q [&'q str]),
    /// The fact is written directly in this scope.
    Scoped(&'q str),
}

/// One workspace-symbol candidate, with everything its ordering needs.
struct Hit<'a> {
    rank: usize,
    /// Lowercased, because the order within a rank is alphabetical and case is not a difference a reader sees.
    name: String,
    /// The same, for the whole qualified name: two `Widget`s in different namespaces are ordered by where they are.
    qualified: String,
    file: &'a Path,
    posting: Posting,
    fact: &'a DeclFact,
}

/// Rank, then **name**, then qualified name, then **file** — answers that compare equal have to come back in one
/// order, and two files may declare the same name — then the position, so that even two overloads in one file do.
fn hit_order(one: &Hit<'_>, two: &Hit<'_>) -> std::cmp::Ordering {
    one.rank
        .cmp(&two.rank)
        .then_with(|| one.name.cmp(&two.name))
        .then_with(|| one.qualified.cmp(&two.qualified))
        .then_with(|| one.file.cmp(two.file))
        .then_with(|| one.posting.cmp(&two.posting))
}

fn sort_hits(hits: &mut [Hit<'_>]) {
    hits.sort_by(hit_order);
}

/// Does this declaration answer to `name`?
///
/// The qualified name first, then the bare one. Both are needed and the order matters: `ns::Widget` written in
/// the query is a claim that the reader knows where the name lives, and honouring it must not be diluted by
/// every unrelated `Widget` elsewhere in the project.
///
/// A **leading `::`** is a third spelling and not a decoration: `::Widget` asks about the global name space, so
/// only a declaration written at file scope answers it. Matching it by dropping the `::` would find `ns::Widget`
/// as well — the exact wrong answer the spelling exists to avoid — so the prefix is honoured rather than
/// stripped.
///
/// A **local** never answers, whatever it is called. See [`DeclFact::local`]: the index is a per-file list and
/// cannot say which function a local belongs to, so a name lookup here has no way to know whether the reader is
/// inside that function — and the answer it would give instead is a *wrong* one, not a missing one, because
/// thousands of locals in the standard library's headers are called `__first` and `n`. Resolving one is the
/// scope tree's job, and it is done there: [`crate::sema::resolve::definition_at`] walks the scopes of the file
/// being edited, which is the only place a local can be placed.
fn matches(fact: &DeclFact, name: &str) -> bool {
    if fact.local {
        return false;
    }

    // **A fact with no name declares no name.** `DeclFact::qualified_name` of a nameless fact *is* its scope, so
    // without this every nameless declaration answers for the class that encloses it — which is how a destructor
    // made `std::vector` ambiguous. Nothing is lost: a nameless fact is still reachable by position, which is
    // what it exists for (`fact_for` records the spelling for the rest).
    if fact.name.is_empty() {
        return false;
    }

    if let Some(global) = name.strip_prefix("::") {
        return fact.scope.is_none() && fact.name == global;
    }

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

/// One declaration a **workspace symbol** search found — see [`ProjectIndex::symbols_matching`].
///
/// Owned rather than borrowed, unlike [`VisibleDeclaration`]: a search collects from several maps (the summaries
/// and the cooked readings) and sorts what it found, so there is nothing to borrow from by the time it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSymbol {
    pub file: PathBuf,
    pub fact: DeclFact,
}

/// How well a declaration matches a search query: lower is better, `None` is not a match.
///
/// # A bare word is about the **name**; a qualified query is about the qualified name's **tail**
///
/// That one rule is what keeps a search for `wid` from answering with every member of every matching class —
/// `ns::Widget::size` contains `wid` too, and two letters in a symbol box should not list a whole project's
/// members — while still letting `Widget::si` find that member, which is the query a user types when they *do* want
/// one.
///
/// ```text
/// `size`         exact, then prefix, then anywhere in the **name**      → finds `Widget::size` at any depth ✓
/// `Widget::si`   matched against the **last two** segments: the ones before the last must match exactly, the last
///                is a prefix → `ns::Widget::size` matches (its tail is `Widget::size`) ✓
/// `ns::widget`   …and `ns::Widget::size`'s tail is `Widget::size`, so `ns` does not match it ✗ — a class query
///                does not drag its members in. It matches `ns::Widget` itself ✓
/// ```
///
/// A leading `::` is dropped, so `::Widget` — how a reader spells "the global one" — finds what `Widget` finds. (The
/// index records a global declaration's qualified name as its bare name, and there is only one to find.)
fn rank_of(qualified: &str, name: &str, wanted: &str) -> Option<usize> {
    let wanted = wanted.trim_start_matches("::").to_lowercase();
    if wanted.is_empty() {
        return None;
    }

    let query: Vec<&str> = wanted.split("::").collect();

    if query.len() == 1 {
        let name = name.to_lowercase();

        if name == wanted {
            return Some(0);
        }
        if name.starts_with(&wanted) {
            return Some(1);
        }
        if name.contains(&wanted) {
            return Some(2);
        }

        return None;
    }

    let segments: Vec<String> = qualified
        .to_lowercase()
        .split("::")
        .map(str::to_string)
        .collect();

    // The **tail**: `Widget::si` has to be able to find `ns::Widget::size`, and it is the last segments it names.
    if segments.len() < query.len() {
        return None;
    }
    let tail = &segments[segments.len() - query.len()..];

    let (last_asked, asked) = query.split_last()?;
    let (last, rest) = tail.split_last()?;

    if rest.iter().map(String::as_str).ne(asked.iter().copied()) {
        return None;
    }

    if last == last_asked {
        return Some(0);
    }
    if last.starts_with(last_asked) {
        return Some(1);
    }

    None
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

    /// **A name only a compiler can see is a name the index answers for — once the file is cooked.**
    ///
    /// `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND` to a compiler and nothing to a reader of the file's
    /// own text, because the declaration is inside the macro's replacement list. So the raw index cannot find
    /// either name, and the cooked reading — indexed and mapped back by `FileIndexer::index_rendering` — can.
    /// This is the whole reason the index holds a second reading.
    #[test]
    fn a_declaration_a_macro_makes_is_found_once_the_file_is_cooked() {
        let files = [
            (
                "/p/decl.h",
                "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                 typedef struct name##__ *name\n",
            ),
            ("/p/api.h", "#include \"decl.h\"\nDECLARE_HANDLE(HWND);\n"),
        ];

        let mut index = ProjectIndex::new();
        let mut definitions = crate::MacroDefinitions::default();
        for (path, source) in files {
            let mut summary = summarize(Path::new(path), source, SummaryKey::new(0, 0));
            for include in &mut summary.includes {
                include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
            }
            let _ = source;
            index.insert(summary);
        }

        let api = Path::new("/p/api.h");
        let source = files[1].1;
        let before = index.definition("HWND__", api);
        assert!(
            matches!(before, Known::Unknown(_)),
            "the raw reading cannot see a declaration inside a macro body: {before:?}"
        );

        // Cook the file **through a unit** — its environment is what makes the macro expand — and hand the index
        // what came out of it.
        let text_of = |wanted: &Path| {
            files
                .iter()
                .find(|(path, _)| Path::new(path) == wanted)
                .map(|(_, source)| *source)
                .unwrap_or("")
        };
        let unit = crate::TranslationUnit::walk(
            index.summary(api).expect("indexed"),
            |wanted| {
                index
                    .summary(wanted)
                    .map(|summary| (summary, text_of(wanted)))
            },
            &crate::Marked::default(),
            &mut definitions,
        );
        let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
        let unit_definitions = unit.definitions();
        let macros = crate::preprocess::cooked::FileMacros::new(
            unit.environment_of(api).expect("the unit reaches api.h"),
            &unit_definitions,
            None,
            true,
        );
        let rendered = crate::preprocess::cooked::cook_with(source, &tokens, &macros).render();

        let provider = crate::MemoryFiles::new();
        let config = crate::CompilerConfig::default();
        let indexer = crate::FileIndexer::new(&provider, &config);
        let indexed = indexer.index_rendering(api, &rendered, SummaryKey::new(0, 0));
        assert_eq!(indexed.mapped.dropped, 0, "every range landed in the file");
        index.insert_cooked(api, indexed.into());

        // Now it can: the struct the macro declared, and the typedef beside it.
        let after = index.definition("HWND__", api);
        let Known::Yes(definition) = after else {
            panic!("the cooked reading declares it: {after:?}");
        };
        assert_eq!(definition.file, api);
        let written = &source[definition.fact.range.start_offset..definition.fact.range.end_offset()];
        assert!(
            written.contains("DECLARE_HANDLE"),
            "the declaration is reported at the invocation: {written:?}"
        );
        assert!(
            matches!(index.definition("HWND", api), Known::Yes(_)),
            "and the typedef with it: {:?}",
            index.definition("HWND", api)
        );
    }

    /// **The two readings do not double-count**: an ordinary declaration is in both, and a candidate list that
    /// held it twice would answer `Ambiguous` for every name in the project.
    #[test]
    fn a_declaration_both_readings_found_is_one_candidate() {
        let source = "struct Plain { int size; };\n";
        let mut index = index(&[("/p/a.h", source)]);

        let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
        let rendered = crate::preprocess::cooked::cook(source, &tokens).render();
        let provider = crate::MemoryFiles::new();
        let config = crate::CompilerConfig::default();
        let indexer = crate::FileIndexer::new(&provider, &config);
        let indexed = indexer.index_rendering(Path::new("/p/a.h"), &rendered, SummaryKey::new(0, 0));
        assert_eq!(indexed.mapped.dropped, 0);
        index.insert_cooked(Path::new("/p/a.h"), indexed.into());

        let found = index.definition("Plain", Path::new("/p/a.h"));
        assert!(matches!(found, Known::Yes(_)), "one candidate: {found:?}");
    }

    /// A cooked reading of `source`, as [`crate::Session::cook`] builds one: the rendering indexed and mapped back.
    fn cooked_reading_of(path: &str, source: &str) -> crate::CookedFile {
        let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
        let rendered = crate::preprocess::cooked::cook(source, &tokens).render();
        let provider = crate::MemoryFiles::new();
        let config = crate::CompilerConfig::default();
        let indexer = crate::FileIndexer::new(&provider, &config);
        indexer
            .index_rendering(Path::new(path), &rendered, SummaryKey::new(0, 0))
            .into()
    }

    /// **A workspace search finds declarations in files nobody has opened**, ranks them, and leaves locals out.
    ///
    /// The query is project-wide and deliberately *not* visibility-scoped: a symbol search is about the project, so
    /// a class in a header nothing includes is still a symbol. What it does share with every other declaration query
    /// is the two readings — a name only the cooked reading declares is a name a compiler knows.
    #[test]
    fn a_workspace_search_finds_symbols_across_the_project() {
        let mut index = index(&[
            ("/p/widget.h", "namespace ns {\nstruct Widget { int size; };\n}\n"),
            (
                "/p/other.cpp",
                "void f() {\n    int widget_local;\n}\nstruct Widest { int y; };\n",
            ),
            ("/p/handle.h", "#define DECLARE_HANDLE(name) struct name##__ { int unused; };\nDECLARE_HANDLE(HWND);\n"),
        ]);

        let names = |found: &[super::ProjectSymbol]| -> Vec<String> {
            found.iter().map(|symbol| symbol.fact.name.clone()).collect()
        };

        // Case-insensitive; both names begin with the query, so they share a rank and are ordered by name.
        let found = index.symbols_matching("wid", 100);
        assert_eq!(names(&found), vec!["Widest", "Widget"], "one rank, alphabetical by name: {found:?}");
        assert_eq!(
            found[1].fact.scope.as_deref(),
            Some("ns"),
            "and the fact says which scope it was written in: {found:?}"
        );

        // The rank comes before the name: the exact match is first although `Widest` sorts earlier than `Widget`.
        let found = index.symbols_matching("widget", 100);
        assert_eq!(names(&found), vec!["Widget"], "{found:?}");
        let found = index.symbols_matching("idg", 100);
        assert_eq!(names(&found), vec!["Widget"], "a substring finds the name it is inside: {found:?}");

        // A local is not a project symbol, even though its name matches.
        assert_eq!(
            names(&index.symbols_matching("widget_local", 100)),
            Vec::<String>::new(),
            "a variable inside a body is not something the project has"
        );

        // A qualified query finds it too — and finds a **member**, which is the one query that should.
        assert_eq!(names(&index.symbols_matching("ns::widget", 100)), vec!["Widget"]);
        assert_eq!(
            names(&index.symbols_matching("Widget::si", 100)),
            vec!["size"],
            "a query with `::` is about a qualified name"
        );
        assert_eq!(index.symbols_matching("wid", 1).len(), 1);

        // **A name only the cooked reading declares is a symbol**: a compiler sees `HWND__`, so a search finds it.
        // The file has to be in the index for the reading to be reachable at all — `insert_cooked` files a *second*
        // reading of a file the index knows.
        index.insert_cooked(
            Path::new("/p/handle.h"),
            cooked_reading_of(
                "/p/handle.h",
                "#define DECLARE_HANDLE(name) struct name##__ { int unused; };\nDECLARE_HANDLE(HWND);",
            ),
        );
        assert!(
            names(&index.symbols_matching("HWND__", 100)).contains(&"HWND__".to_string()),
            "the macro-declared type is a symbol a compiler knows"
        );
    }

    /// **Re-indexing a file with *different* text takes its cooked reading with it.**
    ///
    /// The ordinary invalidation drops both together ([`ProjectIndex::forget`] — an edit, a close, a watched file all
    /// go through it), and this is the path with no `forget` in it: the file was read again and the bytes differ, so
    /// its own declarations are replaced by that insert. A cooked reading left over from the old text would go on
    /// answering for words nobody wrote any more, at offsets into them — a wrong answer rather than a missing one.
    ///
    /// The second half is the guard that keeps this from being a cache defeat: the *same* text re-read (a cache hit,
    /// a re-index of an unchanged file) is not a change, and the reading stays.
    #[test]
    fn re_indexing_different_text_takes_the_cooked_reading_with_it() {
        let source = "struct Plain { int size; };\n";
        let mut index = index(&[("/p/a.h", source)]);
        index.insert_cooked(Path::new("/p/a.h"), cooked_reading_of("/p/a.h", source));
        assert!(index.cooked_declarations(Path::new("/p/a.h")).is_some());

        // The same text, read again: the content hash matches, so the reading describes what is still there.
        let same = summarize(Path::new("/p/a.h"), source, SummaryKey::new(0, 0));
        index.insert(same);
        assert!(
            index.cooked_declarations(Path::new("/p/a.h")).is_some(),
            "an unchanged file keeps its reading"
        );

        // Different text: the reading described the old bytes, so it goes.
        let changed = summarize(
            Path::new("/p/a.h"),
            "struct Other { int size; };\n",
            SummaryKey::new(0, 0),
        );
        index.insert(changed);
        assert!(
            index.cooked_reading(Path::new("/p/a.h")).is_none(),
            "the reading goes with the text it was built from"
        );
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
    fn two_declarations_of_one_name_are_a_list_for_a_consumer_that_shows_one() {
        // The fixture above, through the plural query: `Ambiguous` is the honest answer to "which one", and a
        // client that can show a peek list never has to be told it — both of these are real answers.
        let index = index(&[
            ("/p/one.h", "int count;\n"),
            ("/p/two.h", "int count;\n"),
            (
                "/p/main.cpp",
                "#include \"one.h\"\n#include \"two.h\"\nvoid f() { count = 1; }\n",
            ),
        ]);

        let Known::Yes(found) = index.definitions("count", Path::new("/p/main.cpp")) else {
            panic!("two declarations, both of them answers");
        };

        assert_eq!(found.found.len(), 2);
        assert_eq!(found.conditional, 0);
        assert_eq!(
            found.found[0].file,
            Path::new("/p/one.h"),
            "the order is imposed (by file, then offset), so a client's peek list cannot reorder itself between \
             two identical requests"
        );
        assert_eq!(found.found[1].file, Path::new("/p/two.h"));
    }

    #[test]
    fn an_overload_set_is_a_list_and_a_namespace_is_one_entity() {
        // The two shapes the measurement on a real file produced — 46 identifiers answered `Ambiguous` there, and
        // the lists behind them were 2, 3, 4 … 17 declarations. `find` in `std::basic_string` is seventeen of them,
        // every one an answer; `std` is **58**, all of them the *same* namespace, which the language reopens
        // rather than redeclares. So the list is not simply "what the index found".
        let index = index(&[
            (
                "/p/lib.h",
                "namespace ns {\n  void f(int);\n  void f(double);\n}\n",
            ),
            ("/p/other.h", "namespace ns {\n  void f(char);\n}\n"),
            (
                "/p/main.cpp",
                "#include \"lib.h\"\n#include \"other.h\"\nvoid h() { ns::f(1); }\n",
            ),
        ]);

        let Known::Yes(found) = index.definitions("ns::f", Path::new("/p/main.cpp")) else {
            panic!("an overload set is three declarations, not an ambiguity");
        };
        assert_eq!(found.found.len(), 3);

        let Known::Yes(found) = index.definitions("ns", Path::new("/p/main.cpp")) else {
            panic!("the namespace is declared in both headers");
        };
        assert_eq!(
            found.found.len(),
            1,
            "one namespace, however many files reopen it: a peek list of fifty-eight entries answers a question \
             nobody asked"
        );
    }

    #[test]
    fn a_declaration_behind_a_conditional_include_is_counted_not_offered() {
        // Whether a name reached through an `#if` is in scope depends on macros this layer does not have, so it is
        // not offered as a jump target — and it is not dropped either: "one declaration" and "one declaration and
        // one that might also be there" are different answers.
        let index = index(&[
            ("/p/one.h", "int count;\n"),
            ("/p/two.h", "int count;\n"),
            (
                "/p/main.cpp",
                "#include \"one.h\"\n#ifdef FEATURE\n#include \"two.h\"\n#endif\nvoid f() { count = 1; }\n",
            ),
        ]);

        let Known::Yes(found) = index.definitions("count", Path::new("/p/main.cpp")) else {
            panic!("the unguarded header declares it");
        };

        assert_eq!(found.found.len(), 1);
        assert_eq!(found.found[0].file, Path::new("/p/one.h"));
        assert_eq!(
            found.conditional, 1,
            "and the one that might also be there is counted, so the answer does not claim there is nothing else"
        );
    }

    #[test]
    fn an_include_inside_a_condition_that_is_decided_and_false_is_not_visible() {
        // The rule that turned a 14351-item completion list into a 692-item one. MSVC's `<intrin.h>` includes
        // `<arm_neon.h>`/`<arm64_neon.h>` under `#if defined(_M_ARM64)`, and `<immintrin.h>` pulls in
        // `zmmintrin.h` for AVX-512 — all of them **decided, and false**, for a file compiled for x64. While an
        // `Inactive` edge was downgraded to `Conditional` instead of dropped, every ARM and AVX-512 intrinsic was
        // "visible" from a hello-world file, and therefore offered to the user: 8 of every 10 completion items.
        //
        // `#if 0` is the same shape with nothing to evaluate: decided, false, and no macro knowledge needed.
        let index = index(&[
            ("/p/arm.h", "int arm_intrinsic;\n"),
            (
                "/p/main.cpp",
                "#if 0\n#include \"arm.h\"\n#endif\nvoid f() { }\n",
            ),
        ]);

        let found = index.definition("arm_intrinsic", Path::new("/p/main.cpp"));
        assert!(
            matches!(
                found,
                Known::Unknown(UnknownReason::NotDeclaredHere(_))
            ),
            "the branch was not taken, so the file is not in this translation unit at all: {found:?}"
        );
    }

    #[test]
    fn a_qualified_name_prefers_the_declaration_that_matches_it() {        let index = index(&[
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
        let (index, tree) = analysed_while_typing(files, from, source);

        assert!(
            tree.get_errors().is_empty(),
            "the fixture must parse cleanly: {source:?}"
        );

        (index, tree)
    }

    /// The same, for a file that is **being typed** and therefore need not parse cleanly.
    ///
    /// Separate from [`analysed`] rather than a flag on it, because the two assertions mean different things: a
    /// shape assertion is worthless if the tree it reads came from a file with errors, while an incomplete file is
    /// the *state the completion query exists for* — `w.` is not a program, it is a question. What is deliberately
    /// **not** relaxed is that the querying file is indexed: the index and the tree have to describe the same text
    /// or the offsets in the answer mean something else.
    fn analysed_while_typing(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
    ) -> (ProjectIndex, cpp_parser::CppSyntaxTree) {
        let mut index = index(files);

        let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());

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
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
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
    fn another_files_local_is_never_the_answer() {
        // The wrong answer this exists to prevent, and it is the *common* one rather than a corner: a header's
        // function bodies declare thousands of names (`__first`, `__n`, `_Tp`), and every one of them is a
        // declaration in the index with no scope to place it in. A lookup that matched one would answer with a
        // name the reader cannot see — and, worse, a name that is not the entity the cursor is asking about.
        let source = "#include \"widget.h\"\nvoid f() {\n  helper();\n}\n";
        let (index, tree) = analysed(
            &[(
                "/p/widget.h",
                "void g() {\n  int helper = 0;\n}\nstruct Widget { int size; };\n",
            )],
            "/p/main.cpp",
            source,
        );

        // The fact itself, so that a failure says which half broke: the header's declaration is marked local.
        let header = index.summary(Path::new("/p/widget.h")).expect("indexed");
        let helper = header
            .declarations
            .iter()
            .find(|fact| fact.name == "helper")
            .expect("the header declares `helper`");
        assert!(helper.local, "declared inside `g`'s body");
        assert_eq!(helper.scope, None, "…which is why the scope cannot say so");

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "helper();"),
        );

        assert!(
            matches!(
                found,
                Known::Unknown(UnknownReason::NotDeclaredHere(_)) | Known::No
            ),
            "the header's local is not a declaration of the name this file writes: {found:?}"
        );

        // And the other half, so that the skip is a filter rather than a hole: a *file-scope* declaration in the
        // same header is still found through the index.
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n}\n";
        let (index, tree) = analysed(
            &[(
                "/p/widget.h",
                "void g() {\n  int helper = 0;\n}\nstruct Widget { int size; };\n",
            )],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        let Known::Yes(definition) = found else {
            panic!("`Widget` is at file scope in the header: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/widget.h"));
        assert!(!definition.fact.local);
    }

    #[test]
    fn a_call_resolves_through_the_return_type_of_a_header() {
        // The whole chain, and three of its links are in the header rather than in the buffer: the class, the
        // member function, and the class its return type names. This is what `DeclFact::returns` was added for,
        // and it is also the path a cached summary goes through — the field is written and read by the codec.
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.inner().size = 1;\n}\n";
        let (index, tree) = analysed(
            &[(
                "/p/widget.h",
                "struct Inner {\n  int size;\n};\nstruct Widget {\n  Inner inner();\n};\n",
            )],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
        let found = super::member_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "size = 1;"),
        );

        let Known::Yes(definition) = found else {
            panic!("`w.inner()` returns an `Inner`, which declares `size`: {found:?}");
        };
        assert_eq!(definition.fact.name, "size");
        assert_eq!(
            definition.file,
            Path::new("/p/widget.h"),
            "the member is in the header, like everything else on this chain"
        );
    }

    #[test]
    fn a_name_only_a_header_declares_is_found_through_the_index() {        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n}\n";
        let (index, tree) = analysed(
            &[("/p/widget.h", "struct Widget { int size; };\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
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
    fn a_qualified_name_resolves_into_an_included_header() {
        // The two layers together: the qualifier names a namespace that only the *header* declares, so the
        // single-file layer cannot answer it and the spelling goes to the index — which can, because a fact
        // records the scope it was written in.
        let source = "#include \"a.h\"\nvoid f() {\n  a::Widget w;\n}\n";
        let (index, tree) = analysed(
            &[("/p/a.h", "namespace a {\n  struct Widget { int x; };\n}\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the header's `a::Widget` must be found: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/a.h"));
        assert_eq!(definition.fact.qualified_name(), "a::Widget");
    }

    #[test]
    fn a_qualified_name_is_not_answered_by_a_same_named_declaration_elsewhere() {
        // The wrong answer qualification exists to prevent, one file further out: `b::Widget` is in scope (its
        // header is included), and the cursor asked for `a::Widget`, which is not. Answering with `b`'s would be a
        // jump to a different entity that the user cannot tell apart from the right one.
        let source = "#include \"b.h\"\nvoid f() {\n  a::Widget w;\n}\n";
        let (index, tree) = analysed(
            &[("/p/b.h", "namespace b {\n  struct Widget { int y; };\n}\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "`a::Widget` is not declared anywhere this file can see: {found:?}"
        );
    }

    #[test]
    fn a_global_spelling_does_not_match_a_namespaced_declaration() {
        // `::Widget` means the global name space. Both declarations exist and both are visible, so a matcher that
        // dropped the `::` would have two candidates and no way to choose — the prefix is what chooses.
        let source = "#include \"both.h\"\nvoid f() {\n  ::Widget w;\n}\n";
        let (index, tree) = analysed(
            &[(
                "/p/both.h",
                "struct Widget { int global; };\nnamespace ns {\n  struct Widget { int nested; };\n}\n",
            )],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the global `Widget` is the one asked for: {found:?}");
        };
        assert_eq!(definition.fact.scope, None);
        assert_eq!(definition.fact.name, "Widget");
    }

    // -------------------------------------------------------------------------------------------
    // Member access
    //
    // The first query that needs a *type*. Every fixture here declares the object and its type in the same
    // file, because that is where the interesting failure is — the lookup that would otherwise happen is
    // "find some `size` in scope", and a test that only ever has one `size` in the project cannot tell the
    // difference between typing the object and guessing.
    // -------------------------------------------------------------------------------------------

    /// The member a `needle` position names, with the file analysed and indexed the way a real query has it.
    fn member_of(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
        needle: &str,
    ) -> Known<super::ProjectDefinition> {
        let (index, tree) = analysed(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        super::member_across_files(&index, &scopes, &root, Path::new(from), at(source, needle))
    }

    // -------------------------------------------------------------------------------------------
    // typedef / using aliases
    //
    // The shape the standard library is written in: `std::string` **is** `std::basic_string<char>` and has no
    // members of its own, so a lookup that stops at the spelling finds nothing at all. The motivating case is in
    // these tests in its real form — a `typedef` inside a namespace, with the target written relative to it.
    // -------------------------------------------------------------------------------------------

    #[test]
    fn a_member_of_an_alias_resolves_through_it() {
        let source = "struct Widget {\n  int size;\n};\nusing Alias = Widget;\n\
                      void f() {\n  Alias w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Alias` names `Widget`, so `size` is `Widget::size`: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
    }

    #[test]
    fn a_member_of_the_standard_librarys_string_resolves_into_basic_string() {
        // The case: `basic_string` is where every member is declared, and the
        // alias's target is written **unqualified** inside `namespace std`, so following it needs the alias's own
        // scope. Getting that wrong looks up a global `basic_string` and finds nothing.
        let source = "namespace std {\n  template<typename T> struct basic_string {\n    int size;\n    \
                      int find(int);\n  };\n  typedef basic_string<char> string;\n}\n\
                      void f() {\n  std::string s;\n  s.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`std::string::size` is `std::basic_string::size`: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(
            member.fact.scope.as_deref(),
            Some("std::basic_string"),
            "the member is declared in the class the alias points at, and its scope says so"
        );
    }

    #[test]
    fn an_alias_written_with_using_resolves_the_same_way() {
        let source = "namespace std {\n  template<typename T> struct basic_string {\n    int size;\n  };\n  \
                      using string = basic_string<char>;\n}\n\
                      void f() {\n  std::string s;\n  s.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the `using` form is the same declaration shape: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("std::basic_string"));
    }

    #[test]
    fn an_alias_target_written_with_a_qualifier_is_taken_as_it_stands() {
        // A qualified target is already a complete spelling, so the alias's scope is not prepended: doing it would
        // ask for `ns::other::Thing`, which nothing declares.
        let source = "namespace other {\n  struct Thing {\n    int size;\n  };\n}\n\
                      namespace ns {\n  using Alias = other::Thing;\n}\n\
                      void f() {\n  ns::Alias t;\n  t.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the qualified target is the class: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("other::Thing"));
    }

    #[test]
    fn an_alias_to_an_alias_is_followed_to_the_class() {
        // Two steps, which is what the bound exists for: `A` → `B` → the class.
        let source = "struct Widget {\n  int size;\n};\nusing B = Widget;\nusing A = B;\n\
                      void f() {\n  A w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("two aliases deep is still a class at the end: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
    }

    #[test]
    fn a_cycle_of_aliases_terminates_and_says_nothing_was_declared() {
        // `using A = B; using B = A;` is writable, so the walk has to stop on its own rather than follow the pair
        // for ever. The answer is the honest one: nothing in the chain names a class.
        let source = "using A = B;\nusing B = A;\nvoid f() {\n  A w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(_)),
            "a cycle of aliases names no class: {found:?}"
        );
    }

    #[test]
    fn an_alias_to_itself_terminates() {
        let source = "using A = A;\nvoid f() {\n  A w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(_)),
            "`using A = A;` names no class: {found:?}"
        );
    }

    #[test]
    fn an_alias_whose_target_is_not_indexed_reports_the_target() {
        // The target is a real spelling even when nobody indexed it, and the answer says so rather than reporting
        // the alias: `Widget::size` is the question that could not be answered, not `Alias::size`.
        let source = "using Alias = Widget;\nvoid f() {\n  Alias w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Unknown(UnknownReason::NotDeclaredHere(written)) = found else {
            panic!("the target is what was looked for: {found:?}");
        };
        assert_eq!(&*written, "Widget::size");
    }

    #[test]
    fn an_alias_to_a_non_class_is_not_a_member_lookup() {
        // `typedef int MyInt;` — following it lands on `int`, which names no class, so the answer is "not
        // declared" rather than a jump into whatever `int` might be mistaken for.
        let source = "typedef int MyInt;\nvoid f() {\n  MyInt x;\n  x.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "`int` declares no members: {found:?}"
        );
    }

    #[test]
    fn a_function_pointer_typedef_records_the_whole_type() {
        // `typedef void (*F)(int);` — the type is the specifiers *and* the declarator with the alias's own name
        // cut out, which is `void (*)(int)`. Reading only the specifiers would give `void`, and reading the
        // declarator without cutting would give a type whose *name* is `F`.
        let source = "typedef void (*F)(int);\nvoid f() {\n  F callback;\n  callback();\n}\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let Known::Yes(fact) = index.definition("F", Path::new("/p/a.cpp")) else {
            panic!("the alias is a fact")
        };
        assert_eq!(
            fact.fact.type_of.as_deref(),
            Some("void (*)(int)"),
            "specifiers plus declarator minus the name"
        );

        // And a `typedef` of a plain type needs no such surgery.
        let plain = "typedef basic_string<char> String;\n";
        let (index, tree) = analysed(&[], "/p/b.cpp", plain);
        let Known::Yes(fact) = index.definition("String", Path::new("/p/b.cpp")) else {
            panic!("the alias is a fact")
        };
        assert_eq!(fact.fact.type_of.as_deref(), Some("basic_string<char>"));

        let _ = (root, tree);
    }

    #[test]
    fn a_member_list_of_an_alias_is_the_member_list_of_its_target() {
        // The other query that turns a spelling into a class, and it has to agree with the single-member one:
        // completion for `std::string` that listed nothing would be a member list of the alias rather than of the
        // class, and an alias has no members.
        let source = "namespace std {\n  template<typename T> struct basic_string {\n    int size;\n    \
                      int find(int);\n  };\n  typedef basic_string<char> string;\n}\n";
        let list = members_of_class(&[], "/p/a.cpp", source, "std::string");

        let Known::Yes(list) = list else {
            panic!("the alias's target is a class with members: {list:?}");
        };
        assert!(
            names(&list).contains(&"size") && names(&list).contains(&"find"),
            "the members are `basic_string`'s: {:?}",
            names(&list)
        );
    }

    #[test]
    fn a_member_access_resolves_through_the_objects_type() {        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Widget::size` is declared in this file: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
        assert!(
            member.fact.range.start_offset < at(source, "void f"),
            "the jump goes to the member's declaration, not to the use"
        );
    }

    #[test]
    fn a_member_of_another_class_with_the_same_name_is_not_the_answer() {
        // The wrong answer this query exists to avoid: two classes with a `size`, and the object decides which
        // one. A lookup that ignored the object would answer with whichever was declared first.
        let source = "struct Other {\n  int size;\n};\nstruct Widget {\n  int size;\n};\n\
                      void f() {\n  Widget w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Widget::size` is the one the object names: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
        assert!(
            member.fact.range.start_offset > at(source, "struct Widget"),
            "the jump lands in `Widget`, not in `Other`"
        );
    }

    #[test]
    fn a_member_access_reads_a_pointer_the_same_way() {
        // `ptr->member` names a member of the pointee. The type spelling of `Widget* p` is `Widget*`, and the
        // pointer is a fact about the declarator rather than about which class the name refers to.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget* p;\n  p->size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the pointee's member is the answer: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
    }

    #[test]
    fn a_member_access_reaches_a_class_in_an_included_header() {
        // The everyday case: the class is in the header, the object is local to the `.cpp`.
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
        let (index, tree) = analysed(
            &[("/p/widget.h", "struct Widget {\n  int size;\n};\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
        let found = super::member_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "size = 1;"),
        );

        let Known::Yes(member) = found else {
            panic!("the member declared in the header is the answer: {found:?}");
        };
        assert_eq!(member.file, Path::new("/p/widget.h"));
        assert_eq!(member.fact.name, "size");
    }

    #[test]
    fn a_member_access_through_a_qualified_type_resolves() {
        // `ns::Widget` as a type: the type spelling goes through the qualified-name machinery rather than being
        // treated as a plain name, which is what makes the two features compose instead of each needing its own
        // path.
        let source = "namespace ns {\n  struct Widget {\n    int size;\n  };\n}\n\
                      void f() {\n  ns::Widget w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`ns::Widget::size` is in this file: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("ns::Widget"));
    }

    #[test]
    fn a_member_of_a_class_that_does_not_have_it_is_not_declared_here() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.nope = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "nope = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "the class is known and the member is not in it: {found:?}"
        );
    }

    #[test]
    fn a_member_access_on_a_call_is_the_callees_return_type() {
        // `make().size` — the construct `DeclFact::returns` exists for.
        //
        // This test used to assert the *opposite*, and its comment said so: "the boundary of this layer, stated as
        // an answer rather than as a wrong guess: `f().size` has a type, and working it out is the larger problem
        // the type layer will have to take on". That problem is now solved for a call whose callee is a
        // declaration this analysis can find, so the boundary moved — the failure of this test is what said so.
        let source = "struct Widget {\n  int size;\n};\nWidget make();\n\
                      void f() {\n  make().size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(definition) = found else {
            panic!("`make()` returns a `Widget`, so `size` is its member: {found:?}");
        };
        assert_eq!(definition.fact.name, "size");
        assert_eq!(definition.fact.type_of.as_deref(), Some("int"));

        // The other reading of the same tokens: `Widget()` is a **temporary** of the class rather than a call of
        // anything, and the declaration is what says which one this is.
        let constructed = member_of(
            &[],
            "/p/a.cpp",
            "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget().size = 1;\n}\n",
            "size = 1;",
        );
        assert!(
            matches!(constructed, Known::Yes(_)),
            "a temporary of a known class has the class's members: {constructed:?}"
        );
    }

    #[test]
    fn a_member_access_through_a_dereference_resolves() {
        // **`(*make()).size` and `(*p).size`** — the dereference, which is arithmetic on a spelling rather than a
        // lookup: the pointer's declaration already says what it points at.
        //
        // This test asserted the *opposite* until the dereference was implemented, and its comment said so: "the
        // object is a dereference, not a call … a dereference, a subscript and an arithmetic expression each need
        // a type *computed* rather than read off a declaration". Two of the three are now computed, so the
        // boundary moved — the failure of this test is what said so, which is the way this file records a
        // capability landing.
        let through_a_call = member_of(
            &[],
            "/p/a.cpp",
            "struct Widget {\n  int size;\n};\nWidget* make();\nvoid f() {\n  (*make()).size = 1;\n}\n",
            "size = 1;",
        );
        let Known::Yes(found) = through_a_call else {
            panic!("`make()` returns a `Widget*`, so `(*make())` is a `Widget`: {through_a_call:?}");
        };
        assert_eq!(found.fact.name, "size");

        // …and the same spelling through a declaration in the file: `Widget* p;`.
        let through_a_pointer = member_of(
            &[],
            "/p/a.cpp",
            "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget* p;\n  (*p).size = 1;\n}\n",
            "size = 1;",
        );
        let Known::Yes(found) = through_a_pointer else {
            panic!("`p` points at a `Widget`: {through_a_pointer:?}");
        };
        assert_eq!(found.fact.name, "size");

        // …and the same spelling through a **parameter**, whose declarator is written inside the parameter list
        // rather than at the top of a declaration: `void f(Widget* p)`.
        let through_a_parameter = member_of(
            &[],
            "/p/a.cpp",
            "struct Widget {\n  int size;\n};\nvoid f(Widget* p) {\n  (*p).size = 1;\n}\n",
            "size = 1;",
        );
        let Known::Yes(found) = through_a_parameter else {
            panic!("`p` points at a `Widget`: {through_a_parameter:?}");
        };
        assert_eq!(found.fact.name, "size");

        // A reference is the other operator that says "the object is elsewhere", and `*` on it gives the same
        // answer — the declaration spells the type the same way up to the operator.
        let through_a_reference = member_of(
            &[],
            "/p/a.cpp",
            "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget& r = w;\n  (&r)->size = 1;\n}\n",
            "size = 1;",
        );
        assert!(
            matches!(through_a_reference, Known::Yes(_)),
            "`&r` is a `Widget*`: {through_a_reference:?}"
        );
    }

    #[test]
    fn a_dereference_of_something_that_is_not_a_pointer_is_an_unknown_type() {
        // The boundary that is left, and it is a *refusal* rather than a wrong answer: `int* p` dereferenced is
        // an `int`, and an `int` has no members — so the member lookup on it says "not declared here", which is
        // the honest thing to tell a consumer. What must not happen is an invented type.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  int* q;\n  (*q).size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "an `int` has no members, and saying so beats inventing a class: {found:?}"
        );

        // The other refusal: a subscript on a *class* is `operator[]`, and reaching its element type means
        // instantiating the template. Nothing about the spelling says which argument that is — `vector`'s is the
        // first and `map`'s is the second — so this answers `Unknown` rather than guessing.
        let vector = member_of(
            &[],
            "/p/a.cpp",
            "struct Widget {\n  int size;\n};\ntemplate<typename T> struct Box { T first; };\n\
             void f() {\n  Box<Widget> box;\n  box[0].size = 1;\n}\n",
            "size = 1;",
        );
        assert!(
            matches!(vector, Known::Unknown(UnknownReason::UnknownType(_))),
            "a class subscript needs the template instantiated: {vector:?}"
        );
    }

    #[test]
    fn a_member_access_through_a_subscript_of_an_array_resolves() {
        // `arr[0].size` — an array's element type is written in the declaration: `Widget[4]` is four `Widget`s,
        // and the subscript takes the last `[…]` off.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget arr[4];\n  arr[0].size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(found) = found else {
            panic!("`arr[0]` is a `Widget`: {found:?}");
        };
        assert_eq!(found.fact.name, "size");

        // An index that is not a literal is the same question, and so is the second dimension of an array of
        // arrays: the subscript takes *one* pair of brackets off, outermost last.
        let two_dimensional = member_of(
            &[],
            "/p/a.cpp",
            "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget grid[2][3];\n  grid[i][j].size = 1;\n}\n",
            "size = 1;",
        );
        assert!(
            matches!(two_dimensional, Known::Yes(_)),
            "`grid[i][j]` is a `Widget`: {two_dimensional:?}"
        );
    }

    #[test]
    fn a_call_of_something_with_no_return_type_is_an_unknown_type() {
        // The boundary inside the new capability, and it is the *honest* answer rather than a wrong guess: a
        // deduced `auto` is not a class anything can be looked up in, so the file never says what this call has.
        let source = "struct Widget {\n  int size;\n};\nauto make() { return Widget{}; }\n\
                      void f() {\n  make().size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
            "the return type is deduced, so the file does not state it: {found:?}"
        );
    }

    #[test]
    fn a_member_access_on_an_unknown_object_is_an_unknown_type() {
        // The object is a name, but nothing declares it — so there is no type to read, and the answer has to say
        // *that* rather than "the member is missing".
        let source = "void f() {\n  widget.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
            "nothing says what `widget` is: {found:?}"
        );
    }

    #[test]
    fn a_cursor_that_is_not_on_a_member_access_has_nothing_to_look_up() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "= 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "the cursor is not on a member name: {found:?}"
        );
    }

    #[test]
    fn a_nested_member_access_follows_the_types_down() {
        // This test used to assert the opposite — that `a.b.size` is `Unknown`, because inferring a nested
        // object's type was not done. It is done now, and this is what it bought: the inner access is typed by the
        // same query, so the outer one is answered by asking the inner one what it is.
        let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n};\n\
                      void f() {\n  Outer a;\n  a.b.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Inner::size` is what `a.b.size` names: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Inner"));
        assert!(
            member.fact.range.start_offset < at(source, "struct Outer"),
            "the jump lands in `Inner`, not in `Outer`"
        );
    }

    #[test]
    fn a_nested_member_access_that_disagrees_with_the_outer_class_is_not_answered_by_it() {
        // `Outer` has a `size` of its own *and* a member whose type has one: the answer is the inner one, because
        // the object is `a.b`. A lookup that fell back to the outer class when the object was not a plain name
        // would land on `Outer::size`, which is the wrong entity and looks plausible.
        let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n  int size;\n};\n\
                      void f() {\n  Outer a;\n  a.b.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the member of `a.b`'s type is the answer: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Inner"));
    }

    #[test]
    fn this_resolves_to_the_enclosing_class() {
        // `this` needs no inference at all: the scope chain already knows which class it is, which is why it is
        // the cheapest case in the type layer and the first one to handle.
        let source = "struct Widget {\n  int size;\n  void grow() {\n    this->size = 1;\n  }\n};\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`this->size` is `Widget::size`: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
    }

    #[test]
    fn this_resolves_to_the_nearest_class_when_classes_are_nested() {
        let source = "struct Outer {\n  int size;\n  struct Inner {\n    int size;\n    void f() {\n      \
                      this->size = 1;\n    }\n  };\n};\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`this` inside `Inner` is `Inner`: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Outer::Inner"));
    }

    #[test]
    fn a_nested_access_propagates_why_the_inner_one_failed() {
        // The inner access is where the trouble is, and its reason is kept rather than flattened: there is no
        // member `nope` in `Outer`, so the inner one has no type — and saying "`Outer` has no `nope`" tells a
        // consumer what to fix, where "the type of `a.nope` is unknown" would only say that something is wrong.
        // (This test's first expectation was `UnknownType`, which is what flattening would produce.)
        let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n};\n\
                      void f() {\n  Outer a;\n  a.nope.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "there is no `nope` to type, and that is the answer worth keeping: {found:?}"
        );
    }

    #[test]
    fn a_member_inherited_from_a_base_class_resolves() {
        // The everyday OO case: the member is not in the class the object's type names, it is in the class that
        // one inherits from. Without the base walk this is `Unknown(NotDeclaredHere)`, which is the answer a user
        // would see for most of their code.
        let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int other;\n};\n\
                      void f() {\n  Derived d;\n  d.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("`Base::size` is inherited by `Derived`: {found:?}");
        };
        assert_eq!(member.fact.name, "size");
        assert_eq!(member.fact.scope.as_deref(), Some("Base"));
        assert!(
            member.fact.range.start_offset < at(source, "struct Derived"),
            "the jump lands in the base, not in the derived class"
        );
    }

    #[test]
    fn a_member_of_the_class_itself_hides_one_in_a_base() {
        // Level by level, and the class's own level is first: a `size` written in `Derived` wins over `Base`'s,
        // which is what the language does and what a reader expects.
        let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int size;\n};\n\
                      void f() {\n  Derived d;\n  d.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the class's own member hides the one it inherits: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Derived"));
    }

    #[test]
    fn a_member_inherited_through_another_base_resolves() {
        // Two levels: `Derived` has nothing, `Middle` has nothing, `Base` has it.
        let source = "struct Base {\n  int size;\n};\nstruct Middle : public Base {\n  int m;\n};\n\
                      struct Derived : public Middle {\n  int d;\n};\n\
                      void f() {\n  Derived x;\n  x.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("a member two bases down is still inherited: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Base"));
    }

    #[test]
    fn a_member_declared_in_two_bases_at_once_is_ambiguous_rather_than_picked() {
        // A diamond without `virtual`: both sides declare `size`, so the name is not uniquely resolved by the
        // language. Picking the first would be a jump to an entity the user cannot tell from the other, which is
        // exactly the wrong answer this project refuses to give.
        let source = "struct Left {\n  int size;\n};\nstruct Right {\n  int size;\n};\n\
                      struct Both : public Left, public Right {\n  int own;\n};\n\
                      void f() {\n  Both b;\n  b.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::Ambiguous(_))),
            "two bases declare it and nothing chooses: {found:?}"
        );
    }

    #[test]
    fn a_member_inherited_across_a_header_resolves() {
        // The base is in the header and the derived class is in the file being edited: the base *list* comes from
        // this file's tree and the base *class* from the index, which is the two halves meeting.
        let source = "#include \"base.h\"\nstruct Derived : public Base {\n  int d;\n};\n\
                      void f() {\n  Derived x;\n  x.size = 1;\n}\n";
        let (index, tree) = analysed(
            &[("/p/base.h", "struct Base {\n  int size;\n};\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
        let found = super::member_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "size = 1;"),
        );

        let Known::Yes(member) = found else {
            panic!("the inherited member is declared in the header: {found:?}");
        };
        assert_eq!(member.file, Path::new("/p/base.h"));
        assert_eq!(member.fact.name, "size");
    }

    #[test]
    fn a_member_nobody_declares_is_still_not_declared() {
        // The base walk must not turn "not found" into "found somewhere": the honest answer survives it.
        let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int d;\n};\n\
                      void f() {\n  Derived x;\n  x.nope = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "nope = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "neither the class nor its base declares it: {found:?}"
        );
    }

    #[test]
    fn a_base_list_is_read_without_its_access_keywords() {
        // `public Base` is one base called `Base`: reading the whole specifier would look for a class named
        // "public Base", which is a name nothing declares.
        let source = "struct Base {\n  int size;\n};\nstruct Derived : private virtual Base {\n  int d;\n};\n\
                      void f() {\n  Derived x;\n  x.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("the access and `virtual` are not part of the base's name: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("Base"));
    }

    #[test]
    fn a_base_that_inherits_from_itself_terminates() {
        // A cycle in the base list cannot be written in valid C++, and a malformed file can produce one: the
        // visited set is what keeps the walk from being an infinite loop on a file that is being typed.
        let source = "struct A : public B {\n  int a;\n};\nstruct B : public A {\n  int size;\n};\n\
                      void f() {\n  A x;\n  x.size = 1;\n}\n";
        let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

        let Known::Yes(member) = found else {
            panic!("a cycle must not stop the member being found: {found:?}");
        };
        assert_eq!(member.fact.scope.as_deref(), Some("B"));
    }

    #[test]
    fn a_local_declaration_still_wins_when_the_header_also_has_the_name() {
        // `count` is declared both here and in the header. The answer is this file's, and it is reached without
        // the index — which is the resolution order, not an optimisation.
        let source = "#include \"widget.h\"\nint count = 0;\nvoid f() {\n  count = 1;\n}\n";
        let (index, tree) = analysed(&[("/p/widget.h", "int count = 7;\n")], "/p/main.cpp", source);

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);
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

    // -------------------------------------------------------------------------------------------
    // Member lists
    //
    // The *list* form of the member query, and the fixtures are written to make the one decision it makes
    // visible: which class declares each member, and whether that is the type asked about or a base. The
    // staleness test at the end is the reason none of it is stored.
    // -------------------------------------------------------------------------------------------

    /// The members of `class`, with the file analysed and indexed the way a real query has them.
    fn members_of_class(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
        class: &str,
    ) -> Known<super::MemberList> {
        let (index, tree) = analysed(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        super::members_of(&index, &scopes, &root, Path::new(from), class)
    }

    /// The member names, in the order the query produced them.
    fn names(list: &super::MemberList) -> Vec<&str> {
        list.members
            .iter()
            .map(|member| member.fact.name.as_str())
            .collect()
    }

    #[test]
    fn a_class_declared_more_than_once_reports_the_bases_it_could_not_read() {
        // MSVC's `<istream>`, in miniature: the class, and an explicit instantiation of it. The base walk asks for
        // **one** declaration — an ambiguous name answers no base list — so the members inherited from `Base` are
        // missing, and the list has to say so instead of looking complete.
        //
        // Measured on the real header: `std::basic_istream` is declared three times (the class and two
        // `template class _CRTIMP2_PURE_IMPORT basic_istream<char, …>;` lines), `members_of` answered its 42 own
        // members, and `eof` — inherited from `basic_ios` — was absent while `unlisted` stayed **empty**.
        let source = "template <class E>\nstruct Stream : Base {\n  int read;\n};\ntemplate struct Stream<char>;\n";
        let found = members_of_class(
            &[("/p/base.h", "struct Base { int eof; };\n")],
            "/p/a.cpp",
            source,
            "Stream",
        );

        let Known::Yes(list) = found else {
            panic!("`Stream` is declared here: {found:?}");
        };
        assert_eq!(names(&list), ["read"], "its own member is listed");
        assert_eq!(
            list.unlisted
                .iter()
                .map(|base| base.spelling.as_str())
                .collect::<Vec<_>>(),
            ["Stream"],
            "and the class whose bases could not be read is named, so the list does not claim completeness"
        );
    }

    #[test]
    fn a_class_lists_the_members_written_in_it() {
        // Written out of alphabetical order on purpose: a member list is ordered by name rather than by the text,
        // because the file's scope tree keeps its bindings name-sorted while the index keeps facts in offset
        // order, and the two have to agree for a class to list the same way wherever it is declared.
        let source = "struct Widget {\n  void grow();\n  int size;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Widget");

        let Known::Yes(list) = found else {
            panic!("`Widget` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["grow", "size"]);
        assert!(
            list.unlisted.is_empty(),
            "a class with no bases has nothing left unread: {:?}",
            list.unlisted
        );

        let size = &list.members[1];
        assert_eq!(size.declared_in, "Widget");
        assert_eq!(size.depth, 0, "its own member, not an inherited one");
        assert!(!size.ambiguous);
        assert_eq!(size.file, Path::new("/p/a.cpp"));
        assert_eq!(
            size.fact.type_of.as_deref(),
            Some("int"),
            "the type a completion shows beside the name"
        );
        assert_eq!(list.own().count(), 2, "every member here is the class's own");
    }

    #[test]
    fn a_derived_class_lists_its_own_members_before_the_ones_it_inherits() {
        // Nearest first, which is the order C++ hides in — and the base is tagged rather than flattened into the
        // derived class, so a consumer can show an inherited member as inherited and can jump to `Base::count`
        // rather than to a copy of it.
        let source = "struct Base {\n  int inherited_count;\n};\n\
                      struct Derived : public Base {\n  int own_count;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in this file: {found:?}");
        };

        assert_eq!(
            names(&list),
            ["own_count", "inherited_count"],
            "the class's own members come first, and the inherited one is last"
        );
        assert_eq!(list.members[0].declared_in, "Derived");
        assert_eq!(list.members[0].depth, 0);
        assert_eq!(
            list.members[1].declared_in, "Base",
            "the member is the base's, not the derived class's"
        );
        assert_eq!(list.members[1].depth, 1);
        assert_eq!(
            list.members[1].fact.type_of.as_deref(),
            Some("int"),
            "and it carries its own declaration's type"
        );
        assert_eq!(list.at_depth(1).count(), 1);
    }

    #[test]
    fn a_member_the_class_redeclares_hides_the_one_in_its_base() {
        // Hiding is by name, not by signature: `Derived::size` hides `Base::size` whatever the parameters are,
        // so the base's is not in the list. A list that kept both would offer a name the language does not find.
        let source = "struct Base {\n  int size;\n};\n\
                      struct Derived : public Base {\n  double size;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["size"], "one `size`, and it is the derived class's");
        assert_eq!(list.members[0].declared_in, "Derived");
        assert_eq!(list.members[0].fact.type_of.as_deref(), Some("double"));
    }

    #[test]
    fn a_name_two_bases_declare_is_listed_twice_and_marked_ambiguous() {
        // The list form of the ambiguity the single-member query reports: both declarations are kept, because
        // dropping either would be choosing one for the user, and both are flagged, because a consumer offering
        // the name has to be able to say that the language does not resolve it here.
        let source = "struct Left {\n  int value;\n};\nstruct Right {\n  int value;\n};\n\
                      struct Both : public Left, public Right {\n  int own;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Both");

        let Known::Yes(list) = found else {
            panic!("`Both` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["own", "value", "value"]);

        let values: Vec<&super::ProjectMember> = list
            .members
            .iter()
            .filter(|member| member.fact.name == "value")
            .collect();
        assert_eq!(values.len(), 2, "both declarations survive into the list");
        assert!(
            values.iter().all(|member| member.ambiguous),
            "both are contested: {values:?}"
        );

        let mut declaring: Vec<&str> = values
            .iter()
            .map(|member| member.declared_in.as_str())
            .collect();
        declaring.sort_unstable();
        assert_eq!(declaring, ["Left", "Right"]);

        assert!(
            !list.members[0].ambiguous,
            "`own` is declared once, in one class"
        );
    }

    #[test]
    fn a_member_whose_name_is_not_recorded_does_not_hide_one_in_a_base() {
        // `virtual ~Base();` is what makes this real rather than hypothetical: the specifier sequence is what lets
        // the declaration through the scope walker, and the name it binds is `~Base` — a destructor, whose
        // `identifier_text()` is `None`, so the fact stores an **empty** name. Comparing two empty names made
        // `~Derived` hide `~Base`, which is a wrong answer arrived at by treating a missing spelling as if it were
        // a spelling: the two differ by the class they name, and neither one's spelling is in the fact.
        let source = "struct Base {\n  virtual ~Base();\n  int size;\n};\n\
                      struct Derived : public Base {\n  virtual ~Derived();\n  int d;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in this file: {found:?}");
        };

        let unnamed: Vec<&super::ProjectMember> = list
            .members
            .iter()
            .filter(|member| member.fact.name.is_empty())
            .collect();
        assert_eq!(
            unnamed.len(),
            2,
            "both destructors are declarations that exist, and dropping one would be a silent omission: {:?}",
            names(&list)
        );

        let mut classes: Vec<&str> = unnamed
            .iter()
            .map(|member| member.declared_in.as_str())
            .collect();
        classes.sort_unstable();
        assert_eq!(classes, ["Base", "Derived"]);

        assert!(
            unnamed.iter().all(|member| !member.ambiguous),
            "an empty name is not one name two classes contest: {unnamed:?}"
        );
        assert!(
            list.members
                .iter()
                .any(|member| member.fact.name == "size" && member.declared_in == "Base"),
            "and the named members still cross the inheritance boundary: {:?}",
            names(&list)
        );
    }

    #[test]
    fn a_destructor_without_a_specifier_is_not_a_member_yet() {
        // The boundary, asserted rather than left to be discovered — see "现在答不了什么" in
        // and `a_destructor_without_a_specifier_declares_nothing_yet` in
        // `tests/scopes.rs` for the rule and for what landing it needs.
        //
        // What this test is really pinning is the *other* half: the class is found by its own name. Before the fix
        // in `name_from_text`, a class whose body held a destructor was itself named `~Derived` — its scope was
        // `~Derived`, `d` was filed under that, and `Derived` was never bound. So this fixture used to answer
        // `Unknown(NotDeclaredHere("Derived"))`, which is why it is here as well as in `tests/scopes.rs`.
        let source = "struct Derived {\n  ~Derived();\n  int d;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("the class must be found by its own name: {found:?}");
        };
        assert_eq!(
            names(&list),
            ["d"],
            "`~Derived` is not listed yet: the walker binds no declarator that is not an `InitDeclarator`, so the \
             destructor is not a fact for this query to list"
        );
    }

    #[test]
    fn overloads_in_one_class_are_not_ambiguous() {
        // The other half of the rule above, and the case a count-of-declarations implementation gets wrong:
        // `f` is declared twice and is one name in one class, so nothing about it is contested.
        let source = "struct Widget {\n  void f();\n  void f(int);\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Widget");

        let Known::Yes(list) = found else {
            panic!("`Widget` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["f", "f"], "both overloads are listed");
        assert!(
            list.members.iter().all(|member| !member.ambiguous),
            "one class declaring a name twice is an overload set, not an ambiguity: {:?}",
            list.members
        );
    }

    #[test]
    fn a_cycle_of_bases_terminates_and_each_class_contributes_once() {
        // A base list that cannot be written in valid C++ and can be produced by a malformed file. The visited set
        // is what keeps the walk finite; without it this test does not fail, it does not return.
        let source = "struct A : public B {\n  int a_member;\n};\nstruct B : public A {\n  int b_member;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "A");

        let Known::Yes(list) = found else {
            panic!("`A` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["a_member", "b_member"]);
        assert_eq!(list.members[0].depth, 0);
        assert_eq!(
            list.members[1].depth, 1,
            "`A` is reached again as `B`'s base and is not listed a second time"
        );
    }

    #[test]
    fn a_template_class_lists_the_members_it_was_written_with() {
        // No instantiation, and the answer says so by what it contains: `value` has the type `T`, which is what
        // the class wrote. Instantiating would mean picking an argument, and a member list for `Holder<int>` and
        // `Holder<std::string>` would then be two different lists — which is a `sema` question, not this one.
        let source = "template <typename T>\nstruct Holder {\n  T value;\n  int count;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Holder");

        let Known::Yes(list) = found else {
            panic!("`Holder` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["count", "value"]);
        assert_eq!(
            list.members[1].fact.type_of.as_deref(),
            Some("T"),
            "the parameter as written, not an instantiated type"
        );
        assert!(
            !list.members.iter().any(|member| member.fact.name == "T"),
            "the template parameter is declared in the parameter list, not in the class: {:?}",
            names(&list)
        );
    }

    #[test]
    fn a_type_nothing_declares_has_no_member_list_rather_than_an_empty_one() {
        // The distinction the whole crate is built around, asked of a list: an empty list is a claim that the type
        // has no members, and this analysis cannot make it about a type it has never seen.
        let source = "void f() { }\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Nowhere");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "`Nowhere` is not a type this analysis has seen: {found:?}"
        );
    }

    #[test]
    fn a_class_that_is_empty_has_an_empty_member_list() {
        // The other side of the test above, and the one that needs the *name* to be asked about rather than the
        // members: `struct Empty { };` writes no member and no scope either — see `scopes::class_like` — so a
        // query that read "no members found" as "no class found" would report an empty class as a missing one.
        let source = "struct Empty { };\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Empty");

        let Known::Yes(list) = found else {
            panic!("`Empty` is declared in this file and has no members: {found:?}");
        };
        assert!(list.members.is_empty());
    }

    #[test]
    fn a_member_declared_in_a_nested_class_is_not_a_member_of_the_outer_one() {
        // The scope a fact records is the scope it was written in, so `Inner::x` is a member of `Inner` and the
        // member of `Outer` is the class `Inner`. A list built by name rather than by scope would put `x` in both.
        let source = "struct Outer {\n  struct Inner {\n    int x;\n  };\n  int y;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Outer");

        let Known::Yes(list) = found else {
            panic!("`Outer` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["Inner", "y"]);
        assert_eq!(list.members[0].fact.qualified_name(), "Outer::Inner");
    }

    #[test]
    fn a_member_list_crosses_a_header_and_keeps_the_base_tagged() {
        // The everyday layout: the base is in a header, the derived class is in the file being edited. The base
        // list comes from the buffer's tree and the base's members from the index, and the answer has to name the
        // header as the place to jump to rather than the buffer.
        let source = "#include \"base.h\"\nstruct Derived : public Base {\n  int d;\n};\n";
        let (index, tree) = analysed(
            &[("/p/base.h", "struct Base {\n  int size;\n};\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        let found = super::members_of(&index, &scopes, &root, Path::new("/p/main.cpp"), "Derived");
        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in the buffer: {found:?}");
        };

        assert_eq!(names(&list), ["d", "size"]);
        assert_eq!(list.members[1].file, Path::new("/p/base.h"));
        assert_eq!(list.members[1].declared_in, "Base");
    }

    #[test]
    fn a_base_that_resolves_to_a_template_is_listed_without_instantiating_it() {
        // `public Base<int>` is the class `Base` for the purpose of finding members: the arguments say which type
        // is inherited, not which class declares what. Reading the spelling literally would look for a class named
        // `Base<int>`, which is a name no declaration has.
        let source = "template <typename T>\nstruct Base {\n  int size;\n};\n\
                      struct Derived : public Base<int> {\n  int d;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` is declared in this file: {found:?}");
        };

        assert_eq!(names(&list), ["d", "size"]);
        assert_eq!(list.members[1].declared_in, "Base");
    }

    #[test]
    fn a_base_nothing_declares_is_reported_as_a_gap_rather_than_dropped() {
        // The list is incomplete and says so. Answering with just `d` would be a claim that `Derived` has one
        // member, which is exactly what this analysis cannot know: `Base` is behind an include nobody indexed.
        let source = "#include \"missing.h\"\nstruct Derived : public Base {\n  int d;\n};\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

        let Known::Yes(list) = found else {
            panic!("`Derived` itself is in the buffer: {found:?}");
        };

        assert_eq!(names(&list), ["d"], "what could be listed is listed");
        assert_eq!(list.unlisted.len(), 1, "and what could not is named");
        assert_eq!(list.unlisted[0].spelling, "Base");
        assert!(matches!(list.unlisted[0].reason, UnknownReason::NotDeclaredHere(_)),
            "the reason has to say that the base is not here, not that the list ended: {:?}",
            list.unlisted[0].reason
        );
    }

    #[test]
    fn a_base_of_a_class_in_a_namespace_is_looked_up_in_that_namespace() {
        // The rule that took `examples/std_query.rs` from 7/9 to 9/9, in the shape MSVC's STL has it: `std::map`
        // inherits from `_Tree`, `_Tree` is declared in `<xtree>` inside `_STD_BEGIN` (= `namespace std {`), and
        // the base clause spells it **unqualified**. A base name is looked up from the scope *enclosing* the class
        // and outward, so `_Tree` written inside `std` names `std::_Tree` — and the file-scope `_Tree` below,
        // which declares something else entirely, must not be consulted: the enclosing namespace is a nearer scope
        // and unqualified lookup stops at the first scope that has the name.
        //
        // Both walks are asserted here because both had the same hole and a fix to one is invisible from the
        // other: the list (`members_of`) and the single member (`member_across_files`).
        let lib = "namespace std {\nstruct _Tree {\n  void find();\n};\nstruct map : _Tree {\n  int count;\n};\n}\n";
        let source = "#include \"lib.h\"\nstruct _Tree {\n  void wrong();\n};\nvoid f() {\n  std::map m;\n  m.find();\n  m.wrong();\n}\n";
        let files: &[(&str, &str)] = &[("/p/lib.h", lib)];

        let Known::Yes(list) = members_of_class(files, "/p/a.cpp", source, "std::map") else {
            panic!("`std::map` is in the indexed header");
        };
        assert_eq!(
            names(&list),
            ["count", "find"],
            "`find` comes from the base, and `wrong` is not a member of anything this class inherits"
        );
        assert!(
            list.unlisted.is_empty(),
            "the base was found, so nothing here is a gap: {:?}",
            list.unlisted
        );
        assert_eq!(
            list.members[1].declared_in, "std::_Tree",
            "the answer names the scope the base was found in, not the spelling the class wrote"
        );
        assert_eq!(list.members[1].depth, 1);

        let found = member_of(files, "/p/a.cpp", source, "find();");
        let Known::Yes(found) = found else {
            panic!("`m.find` is `std::_Tree::find`: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/lib.h"));
        assert_eq!(found.fact.qualified_name(), "std::_Tree::find");

        assert!(
            matches!(
                member_of(files, "/p/a.cpp", source, "wrong();"),
                Known::Unknown(UnknownReason::NotDeclaredHere(_))
            ),
            "a base name that a nearer scope has does not fall through to the file-scope `_Tree`"
        );
    }

    #[test]
    fn a_type_spelled_from_the_global_name_space_lists_the_same_members() {
        // `::Widget w;` is a declaration whose type spelling carries the leading `::`, and a consumer feeding this
        // query from `DeclFact.type_of` therefore hands it `::Widget`. The index keys a global declaration under
        // its bare name — a file-scope declaration has no scope prefix — so the prefix has to come off before the
        // walk, or a class that is plainly in the buffer answers `Unknown(NotDeclaredHere("::Widget"))`.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  ::Widget w;\n}\n";
        let found = members_of_class(&[], "/p/a.cpp", source, "::Widget");

        let Known::Yes(list) = found else {
            panic!("`::Widget` is the file-scope `Widget`: {found:?}");
        };
        assert_eq!(names(&list), ["size"]);
        assert_eq!(list.members[0].declared_in, "Widget");
    }

    #[test]
    fn a_member_added_to_a_base_appears_without_reindexing_the_derived_class() {
        // The test that decides whether inherited members are *materialized*, and the reason they are not.
        //
        // Only `b.h` is rebuilt below. Nothing about `A`'s text or its key changes, so a summary that stored the
        // members `A` inherits would go on answering with `old_member` for ever — and no invalidation rule could
        // catch it, because there is nothing to invalidate: the edit is in a file `A` never mentions by name.
        // Walking the chain at query time is what makes the second assertion true.
        let a_header = "#include \"b.h\"\nstruct A : public B {\n  int a_member;\n};\n";
        let before = "struct B {\n  int old_member;\n};\n";
        let after = "struct B {\n  int new_member;\n};\n";
        let main = "#include \"a.h\"\n";

        let mut index = index(&[
            ("/p/main.cpp", main),
            ("/p/a.h", a_header),
            ("/p/b.h", before),
        ]);

        let tree = cpp_parser::CppParser::parse(main, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        let listed = |index: &ProjectIndex| {
            match super::members_of(index, &scopes, &root, Path::new("/p/main.cpp"), "A") {
                Known::Yes(list) => list
                    .members
                    .iter()
                    .map(|member| member.fact.name.clone())
                    .collect::<Vec<_>>(),
                other => panic!("`A` is declared in an included header: {other:?}"),
            }
        };

        assert_eq!(listed(&index), ["a_member", "old_member"]);

        // `b.h` and nothing else. `a.h` keeps the summary it was built with, which is the whole point.
        let mut rebuilt = summarize(Path::new("/p/b.h"), after, SummaryKey::new(1, 0));
        for include in &mut rebuilt.includes {
            include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(rebuilt);

        assert_eq!(
            listed(&index),
            ["a_member", "new_member"],
            "the derived class's members are read from its bases at query time, so the edit is seen without \
             anything of `A`'s being rebuilt"
        );

        // And why that worked, asserted rather than implied: `A`'s own summary holds its own member and nothing it
        // inherits. The day something starts writing inherited members into a derived class's summary, this is
        // where it shows up — before the stale answers do.
        let a = index.summary(Path::new("/p/a.h")).expect("`a.h` is indexed");
        let a_members: Vec<&str> = a
            .declarations
            .iter()
            .filter(|fact| fact.scope.as_deref() == Some("A"))
            .map(|fact| fact.name.as_str())
            .collect();
        assert_eq!(
            a_members,
            ["a_member"],
            "a fact stores the spelling `B`, never the members `B` happens to have"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Completion: the cursor-to-members query
    //
    // Every fixture here is an **incomplete program**, which is the only honest way to test this: `w.` does not
    // parse, and a test that made it parse would be testing a question nobody asks. The helper below therefore
    // does not assert a clean parse — see `analysed_while_typing`.
    // -------------------------------------------------------------------------------------------
    /// The offset just **past** the last `needle`, which is where the cursor is after that much has been typed.
    fn after(source: &str, needle: &str) -> usize {
        source
            .rfind(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not in {source:?}"))
            + needle.len()
    }

    /// The members offered at the cursor just past `needle`.
    fn completions_at(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
        needle: &str,
    ) -> Known<super::MemberCompletions> {
        let (index, tree) = analysed_while_typing(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        super::member_completions_at(
            &index,
            &scopes,
            &root,
            Path::new(from),
            after(source, needle),
        )
    }

    #[test]
    fn a_member_access_with_nothing_typed_offers_the_types_members() {
        // The keystroke that asks the question. `w.` is not a program and does not parse, which is exactly why
        // this is a query over a damaged tree rather than over a compiled one.
        let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                      void f() {\n  Widget w;\n  w.\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "w.");

        let Known::Yes(completions) = found else {
            panic!("`w` is a `Widget` declared two lines up: {found:?}");
        };

        assert_eq!(completions.class, "Widget");
        assert_eq!(
            completions
                .members
                .members
                .iter()
                .map(|member| member.fact.name.as_str())
                .collect::<Vec<_>>(),
            ["grow", "size"]
        );
        assert_eq!(completions.prefix, "", "nothing of the name is written yet");
        assert_eq!(
            completions.member_range.start_offset,
            after(source, "w."),
            "the empty range sits just past the operator, which is where the name goes"
        );
        assert_eq!(completions.member_range.length, 0);
    }

    #[test]
    fn a_half_typed_member_offers_the_same_members_and_names_the_prefix() {
        // The other half of the same question: the list is the *whole* list — filtering is the client's job and
        // it has the document — while the range is what the client replaces with the chosen member.
        let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                      void f() {\n  Widget w;\n  w.si\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "w.si");

        let Known::Yes(completions) = found else {
            panic!("the cursor is on the member being typed: {found:?}");
        };

        assert_eq!(completions.class, "Widget");
        assert_eq!(completions.prefix, "si");
        assert_eq!(
            &source[completions.member_range.start_offset..completions.member_range.end_offset()],
            "si",
            "the range covers what is written, so replacing it does not leave a doubled name"
        );
    }

    #[test]
    fn a_pointer_member_access_offers_the_pointees_members() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget* p;\n  p->\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "p->");

        let Known::Yes(completions) = found else {
            panic!("`Widget* p` is a pointer to a `Widget`: {found:?}");
        };
        assert_eq!(completions.class, "Widget");
        assert_eq!(completions.members.members.len(), 1);
    }

    #[test]
    fn a_completion_on_this_offers_the_enclosing_classs_members() {
        // `this` needs no inference — the scope chain already knows which class it is — which makes it the one
        // completion inside a class body that works before the object has a name.
        let source = "struct Widget {\n  int size;\n  void grow() {\n    this->\n  }\n};\n";
        let found = completions_at(&[], "/p/a.cpp", source, "this->");

        let Known::Yes(completions) = found else {
            panic!("`this` is the enclosing `Widget`: {found:?}");
        };
        assert_eq!(completions.class, "Widget");
        assert_eq!(
            completions
                .members
                .members
                .iter()
                .map(|member| member.fact.name.as_str())
                .collect::<Vec<_>>(),
            ["grow", "size"]
        );
    }

    #[test]
    fn the_members_offered_include_the_ones_the_bases_declare() {
        // Completion is where the query-time base walk pays off: the whole inherited list is there, each entry
        // tagged with the class that declares it, without anything having been stored on `Derived`.
        let source = "struct Base {\n  int inherited_size;\n};\n\
                      struct Derived : public Base {\n  int own_size;\n};\n\
                      void f() {\n  Derived d;\n  d.\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "d.");

        let Known::Yes(completions) = found else {
            panic!("`d` is a `Derived`: {found:?}");
        };

        let listed: Vec<(&str, &str)> = completions
            .members
            .members
            .iter()
            .map(|member| (member.fact.name.as_str(), member.declared_in.as_str()))
            .collect();
        assert_eq!(
            listed,
            [("own_size", "Derived"), ("inherited_size", "Base")],
            "the class's own member first, then the inherited one, tagged with where it is declared"
        );
    }

    #[test]
    fn the_members_offered_cross_a_header() {
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.\n}\n";
        let (index, tree) = analysed_while_typing(
            &[("/p/widget.h", "struct Widget {\n  int size;\n};\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        let found = super::member_completions_at(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            after(source, "w."),
        );

        let Known::Yes(completions) = found else {
            panic!("the class is in the included header: {found:?}");
        };
        assert_eq!(completions.class, "Widget");
        assert_eq!(completions.members.members[0].file, Path::new("/p/widget.h"));
    }

    #[test]
    fn a_completion_after_a_call_offers_what_the_call_returns() {
        // `make().` — the keystroke this capability exists for, and the one the test below is the boundary of.
        // Before `returns` existed this answered `UnknownType`; now it is the same answer as `w.` for a `w` of the
        // same type, which is the whole point of recording a return type at all.
        let source = "struct Widget {\n  int size;\n  void grow();\n};\nWidget make();\n\
                      void f() {\n  make().\n}\n";
        let Known::Yes(completions) = completions_at(&[], "/p/a.cpp", source, "make().") else {
            panic!("`make()` returns a `Widget`");
        };

        assert_eq!(completions.class, "Widget");
        assert_eq!(
            completions
                .members
                .members
                .iter()
                .map(|member| member.fact.name.as_str())
                .collect::<Vec<_>>(),
            ["grow", "size"]
        );
    }

    #[test]
    fn a_completion_after_a_dereference_offers_the_pointees_members() {
        // The completion form of [`a_member_access_through_a_dereference_resolves`], and the one a user meets:
        // typing `(*make()).` should offer the `Widget`'s members, not nothing.
        //
        // This test asserted the *opposite* until the dereference was implemented — "a call's return type is not
        // computed yet" — and the failing assertion is what said the boundary had moved.
        let source = "struct Widget {\n  int size;\n};\nWidget* make();\n\
                      void f() {\n  (*make()).\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "(*make()).");

        let Known::Yes(completions) = found else {
            panic!("`(*make())` is a `Widget`, so its members are the answer: {found:?}");
        };
        assert_eq!(completions.class, "Widget");
    }

    #[test]
    fn a_completion_on_an_expression_with_no_type_offers_nothing_and_says_why() {
        // What is left of the boundary, and this is where a user meets it. Offering the members of some other
        // class would be worse than offering none: the list looks like an answer.
        //
        // An **arithmetic expression** is the case that is still out of reach, and it is the honest one: `a + b`
        // has a type the language computes from conversions this layer does not model, so there is nothing to
        // read off a declaration.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget a;\n  (a.size + a.size).\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "(a.size + a.size).");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
            "an arithmetic expression's type is not read off a declaration: {found:?}"
        );
    }

    #[test]
    fn a_completion_on_an_object_of_an_unknown_type_offers_nothing() {
        // The object resolves fine — `w`'s declaration *says* its type is `Nowhere` — and what fails is the type:
        // nothing here declares it. So the reason names the type rather than the object, which is what tells a
        // consumer whether to go looking for a missing declaration of `w` or of `Nowhere`.
        let source = "void f() {\n  Nowhere w;\n  w.\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "w.");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "`Nowhere` is a type nothing here declares: {found:?}"
        );
        let Known::Unknown(UnknownReason::NotDeclaredHere(name)) = found else {
            unreachable!("just checked")
        };
        assert_eq!(&*name, "Nowhere", "and the reason names the type, not the object");
    }

    #[test]
    fn a_cursor_that_is_not_on_a_member_access_offers_nothing() {
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "Widget w;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "a declaration is not a member access: {found:?}"
        );
    }

    #[test]
    fn a_cursor_on_the_object_asks_about_the_object() {
        // The offset decides, not the node: `w.size` with the cursor *on* `w` is a question about `w`, and
        // answering it with `Widget`'s members would insert a member name in the middle of the object's.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size;\n}\n";
        let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        let found = super::member_completions_at(
            &index,
            &scopes,
            &root,
            Path::new("/p/a.cpp"),
            at(source, "w.size"),
        );

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "the cursor is on the object, before the operator: {found:?}"
        );
    }

    #[test]
    fn a_cursor_inside_a_name_filters_by_the_part_before_it() {
        // The range and the prefix answer two halves of one edit, and this is where they differ: the client
        // replaces all of `size`, while the filter matches `si` — the part the user has actually typed. Using the
        // whole spelling for both is what makes completion offer nothing in the middle of a word.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size;\n}\n";
        let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        // Two characters into `size`.
        let cursor = at(source, "w.size") + 2 + 2;

        let found =
            super::member_completions_at(&index, &scopes, &root, Path::new("/p/a.cpp"), cursor);
        let Known::Yes(completions) = found else {
            panic!("the cursor is inside the member's name: {found:?}");
        };

        assert_eq!(completions.prefix, "si", "what has been typed so far");
        assert_eq!(
            &source[completions.member_range.start_offset..completions.member_range.end_offset()],
            "size",
            "and the whole name is what gets replaced"
        );
    }

    #[test]
    fn a_completed_member_access_still_answers_with_its_own_prefix() {
        // The state between two keystrokes: `w.size` is a finished expression, and a cursor at its end is still
        // asking for the members — with `size` as the prefix to replace. Nothing about the completion path is
        // special to the half-typed case.
        let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                      void f() {\n  Widget w;\n  w.size\n}\n";
        let found = completions_at(&[], "/p/a.cpp", source, "w.size");

        let Known::Yes(completions) = found else {
            panic!("the cursor is at the end of `size`: {found:?}");
        };
        assert_eq!(completions.prefix, "size");
        assert_eq!(
            &source[completions.member_range.start_offset..completions.member_range.end_offset()],
            "size"
        );
    }

    #[test]
    fn a_jump_on_a_member_that_is_not_written_yet_has_nothing_to_jump_to() {
        // The completion query tolerates a nameless access; the *definition* query must not, and the difference is
        // the question rather than the shape. Reading `w.` as "the member named nothing" would answer
        // `NotDeclaredHere("Widget::")`, which says the class is missing a member that has not been typed.
        let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.\n}\n";
        let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        let found = super::member_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/a.cpp"),
            after(source, "w."),
        );

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "there is no written name to resolve: {found:?}"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Completion: the cursor-to-names query
    //
    // The sibling of the member query above, and the same kind of fixture: a *name being written* is not a
    // program, so these files do not parse and are not meant to. What each test asserts is one of the four
    // positions — after a `::`, after a class's `::`, after a global `::`, and with no qualifier at all — because
    // they reach the same query through different scopes.
    // -------------------------------------------------------------------------------------------

    /// The names offered at the cursor just past `needle`.
    fn names_at(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
        needle: &str,
    ) -> Known<super::NameCompletions> {
        let (index, tree) = analysed_while_typing(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        super::name_completions_at(
            &index,
            &scopes,
            &root,
            Path::new(from),
            after(source, needle),
        )
    }

    /// The names offered at the **first offset of the body** of `source` — where nothing is written yet.
    ///
    /// The position with no prefix, which is a different question from [`names_at`]'s and cannot be spelled as one:
    /// a needle is what a test looks for, and "the cursor is at the start of a line" is not a spelling.
    fn names_at_blank(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
    ) -> Known<super::NameCompletions> {
        let (index, tree) = analysed_while_typing(files, from, source);
        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root, &crate::NoMacroBodies);

        let at = source
            .find("void f()")
            .and_then(|body| source[body..].find('{').map(|brace| body + brace + 1))
            .expect("the fixture has a body");

        super::name_completions_at(&index, &scopes, &root, Path::new(from), at)
    }

    /// The names in an answer, in the order the query put them.
    #[track_caller]
    fn offered(found: &Known<super::NameCompletions>) -> Vec<String> {
        match found {
            Known::Yes(completions) => completions
                .names
                .iter()
                .map(|name| name.fact.name.clone())
                .collect(),
            other => panic!("expected names: {other:?}"),
        }
    }

    #[test]
    fn a_qualified_scope_offers_the_names_written_in_it() {
        let source = "namespace ns {\n  struct Widget { };\n  int helper();\n}\nvoid f() {\n  ns::\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "ns::");

        assert_eq!(offered(&found), ["Widget", "helper"], "sorted within the scope");
        let Known::Yes(completions) = found else {
            unreachable!("asserted above")
        };
        assert_eq!(completions.scope, "ns", "the scope as a lookup key");
        assert_eq!(completions.prefix, "", "nothing of the name is typed yet");
        assert_eq!(
            completions.name_range.start_offset,
            after(source, "ns::"),
            "the empty range sits just past the `::`, where the name goes"
        );
        assert_eq!(completions.name_range.length, 0);
    }

    #[test]
    fn a_half_typed_name_keeps_the_prefix_and_replaces_the_whole_segment() {
        // The two halves of one edit, as in the member query: the range covers what the client replaces (`Wid`),
        // and the prefix is what it filters by — the part before the cursor.
        let source = "namespace ns {\n  struct Widget { };\n}\nvoid f() {\n  ns::Wid\n}\n";
        let Known::Yes(completions) = names_at(&[], "/p/a.cpp", source, "ns::Wid") else {
            panic!("the cursor is on the name being typed");
        };

        assert_eq!(completions.scope, "ns");
        assert_eq!(completions.prefix, "Wid");
        assert_eq!(
            completions.name_range.start_offset,
            at(source, "Wid"),
            "the range is the written segment, not the point"
        );
        assert_eq!(completions.name_range.length, 3);
    }

    #[test]
    fn a_class_scope_offers_its_members_and_the_ones_its_bases_declare() {
        // `Derived::` is a scope like `ns::`, and the difference between them is the language's: a class is
        // defined once, and the names it has include the ones it inherits.
        let source = "struct Base {\n  typedef int size_type;\n};\n\
                      struct Derived : Base {\n  typedef int value_type;\n};\n\
                      void f() {\n  Derived::\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "Derived::");

        assert_eq!(
            offered(&found),
            ["value_type", "size_type"],
            "the class's own names first, then the ones a base declares"
        );
        let Known::Yes(completions) = found else {
            unreachable!("asserted above")
        };
        assert_eq!(completions.scope, "Derived");
        assert_eq!(
            completions
                .names
                .iter()
                .map(|name| name.depth)
                .collect::<Vec<_>>(),
            [0, 1],
            "the base's name is one step further out, which is the order lookup walks in"
        );
    }

    #[test]
    fn a_global_scope_offers_only_what_is_declared_at_file_scope() {
        // `::` is a scope and not an empty qualifier: it asks about the global name space, so a namespace member
        // and a local are both *not* in the answer, while the file-scope declarations are — the enclosing function
        // and namespace included, because those are written there.
        let source = "int global;\nnamespace ns {\n  int inner;\n}\nvoid f() {\n  int local;\n  ::\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "::");

        assert_eq!(offered(&found), ["f", "global", "ns"]);
        let Known::Yes(completions) = found else {
            unreachable!("asserted above")
        };
        assert_eq!(completions.scope, "::", "the global name space, as spelled");

        let names = offered(&Known::Yes(completions.clone()));
        assert!(
            !names.contains(&"inner".to_string()) && !names.contains(&"local".to_string()),
            "a namespace member and a local are not in the global name space: {names:?}"
        );
    }

    #[test]
    fn a_bare_name_offers_the_scopes_the_cursor_is_inside_innermost_first() {
        // No qualifier: every name visible from here, and the order is C++'s — the body's own, then the class's,
        // then the namespace's, then the file's.
        let source = "int at_file_scope;\nnamespace ns {\n  int in_a_namespace;\n  struct C {\n    int member;\n    \
                      void g() {\n      int local;\n      loc\n    }\n  };\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "loc");

        assert_eq!(
            offered(&found),
            [
                // depth 0: the body the cursor is in
                "local",
                // depth 1: the class — its members, `g` among them
                "g",
                "member",
                // depth 2: the namespace
                "C",
                "in_a_namespace",
                // depth 3: file scope, which is also where every included file writes
                "at_file_scope",
                "ns"
            ]
        );

        let Known::Yes(completions) = found else {
            unreachable!("asserted above")
        };
        assert_eq!(completions.scope, "", "no qualifier was written");
        assert_eq!(completions.prefix, "loc");

        // The order is not alphabetical and is not meant to be: it is the order C++ searches in, so a consumer
        // that shows the list in this order shows the name the user would get first.
        let depths: Vec<usize> = completions.names.iter().map(|name| name.depth).collect();
        let mut sorted = depths.clone();
        sorted.sort_unstable();
        assert_eq!(depths, sorted, "nearer scopes come first");
        assert_eq!(depths[0], 0, "the local is in the scope the cursor is in");
    }

    #[test]
    fn another_files_local_is_not_offered_by_a_bare_name() {
        // The reason `local` is a field at all. The header's `__first` is a declaration in the index with no scope
        // to place it in, and the standard library's headers declare thousands of names like it — offering one
        // would fill the list with names the user cannot see, and the *right* answer for this cursor is the local
        // that really is here.
        let header = "/p/widget.h";
        let declaration = "void g() {\n  int __first = 0;\n  int helper;\n}\nint global_name;\n";
        let source = "#include \"widget.h\"\nvoid f() {\n  int mine;\n  min\n}\n";
        let found = names_at(&[(header, declaration)], "/p/main.cpp", source, "min");

        let names = offered(&found);
        assert!(
            names.contains(&"mine".to_string()),
            "the local of *this* file is offered, from its own scopes: {names:?}"
        );
        assert!(
            !names.contains(&"__first".to_string()) && !names.contains(&"helper".to_string()),
            "another file's locals are not names this file can write: {names:?}"
        );

        // **A name that does not begin with what is being typed is not collected**, which is a filter and not a
        // ranking: `global_name` cannot be completed from `min`, and a client would drop it. So the same question
        // is asked once more at the **blank** position the feature exists for, which is where the file-scope name
        // has to appear — asking it at `min` would be asserting two things at once.
        let at_a_blank = names_at_blank(&[(header, declaration)], "/p/main.cpp", source);
        let names = offered(&at_a_blank);
        assert!(
            names.contains(&"global_name".to_string()),
            "…while a file-scope name in the same header is offered where nothing is typed: {names:?}"
        );
    }

    #[test]
    fn a_namespace_is_merged_across_the_buffer_and_the_header() {
        // A namespace can be reopened by every file that mentions it, so the buffer's names are never the whole
        // answer — the difference from a class, which is defined once and listed from one place.
        let source = "#include \"ns.h\"\nnamespace ns {\n  int in_the_buffer;\n}\nvoid f() {\n  ns::\n}\n";
        let found = names_at(
            &[("/p/ns.h", "namespace ns {\n  int in_the_header;\n}\n")],
            "/p/main.cpp",
            source,
            "ns::",
        );

        assert_eq!(offered(&found), ["in_the_buffer", "in_the_header"]);
    }

    #[test]
    fn a_scope_nothing_declares_is_not_an_empty_list() {
        // "Declared and empty" and "no such scope" are different answers, and the second one is what stops a
        // consumer from reporting that the user is looking at a namespace with nothing in it.
        let source = "void f() {\n  nowhere::\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "nowhere::");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "nothing declares `nowhere`: {found:?}"
        );
    }

    #[test]
    fn a_cursor_where_a_name_can_be_written_offers_the_visible_names() {
        // **The entry condition, which used to be the whole bug.** A cursor with no name written yet — at the start
        // of a declaration, after `return `, at the beginning of a line — is the position completion exists for.
        // This test used to assert the opposite ("the cursor is on a declaration's name, which is not a name being
        // written"), and a language server that answers nothing when a client asks at a blank position is one a
        // user reports as "it has no completion for local variables" — which is what happened.
        //
        // The replacement range is the *whole* identifier the cursor is on, so choosing a name replaces it rather
        // than inserting beside it.
        // The cursor is just past the name `Widget` — the position a client reports when the user puts the cursor
        // on the identifier and presses the completion key.
        let source = "struct Widget { int size; };\nvoid f() {\n  Widget w;\n}\n";
        let found = names_at(&[], "/p/a.cpp", source, "Widget");

        let Known::Yes(found) = found else {
            panic!("a name can be written here: {found:?}");
        };
        let names: Vec<&str> = found
            .names
            .iter()
            .map(|offered| offered.fact.name.as_str())
            .collect();
        assert!(
            names.contains(&"Widget") && names.contains(&"f"),
            "the names in scope: {names:?}"
        );
        assert_eq!(found.prefix, "Widget", "what is written filters the list");
        assert_eq!(
            found.name_range.length,
            "Widget".len(),
            "and the chosen name replaces it rather than being inserted beside it"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Macros
    //
    // Every test here is about **translation order**: which of the `#define`s and `#undef`s the
    // preprocessor would have gone past last. The fixtures are written so that the *set* of facts is the
    // same in several of them and only their order differs, because the set is what a naive
    // implementation gets right and the order is what it gets wrong.
    // -------------------------------------------------------------------------------------------

    /// The line a found fact is on, so a test can say "the second one" without arithmetic.
    fn line_of(source: &str, offset: usize) -> usize {
        source[..offset].matches('\n').count()
    }

    #[test]
    fn a_macro_defined_above_its_use_resolves_to_the_define() {
        let source = "#define MAX 1\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));

        let Known::Yes(found) = found else {
            panic!("the define above the use is the answer: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/a.cpp"));
        assert_eq!(found.fact.name, "MAX");
        assert!(found.fact.kind.is_definition());
        assert_eq!(
            &source[found.fact.range.start_offset..found.fact.range.end_offset()],
            "MAX",
            "and the range is the name a user would be sent to"
        );
    }

    #[test]
    fn a_macro_defined_below_its_use_is_not_in_force_yet() {
        // The file is read top to bottom, so a `#define` further down the page is not what the name above it
        // means. A table keyed by name alone answers this one wrong, and it is the reason the query is positional.
        let source = "int x = MAX;\n#define MAX 1\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "nothing above the use defines it: {found:?}"
        );
    }

    #[test]
    fn a_macro_in_an_included_header_resolves_into_that_header() {
        let source = "#include \"config.h\"\nint x = FEATURE;\n";
        let header = "#define FEATURE 1\n";
        let (index, tree) = analysed(&[("/p/config.h", header)], "/p/main.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));

        let Known::Yes(found) = found else {
            panic!("the header's define must be found: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/config.h"));
        assert_eq!(line_of(header, found.fact.range.start_offset), 0);
    }

    #[test]
    fn a_header_included_after_the_use_does_not_define_it_yet() {
        let source = "int x = FEATURE;\n#include \"config.h\"\n";
        let (index, tree) = analysed(&[("/p/config.h", "#define FEATURE 1\n")], "/p/main.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "the include is below the use: {found:?}"
        );
    }

    #[test]
    fn the_later_of_two_headers_that_define_it_wins() {
        // The set of facts is identical in this test and the next one; only the order differs. That is the pair a
        // name-keyed implementation cannot tell apart.
        let source = "#include \"one.h\"\n#include \"two.h\"\nint x = FLAG;\n";
        let one = "#define FLAG 1\n";
        let two = "#define FLAG 2\n";
        let (index, tree) = analysed(&[("/p/one.h", one), ("/p/two.h", two)], "/p/main.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FLAG;"));

        let Known::Yes(found) = found else {
            panic!("the last define gone past is the answer: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/two.h"));
    }

    #[test]
    fn swapping_the_two_includes_swaps_the_answer() {
        let source = "#include \"two.h\"\n#include \"one.h\"\nint x = FLAG;\n";
        let one = "#define FLAG 1\n";
        let two = "#define FLAG 2\n";
        let (index, tree) = analysed(&[("/p/one.h", one), ("/p/two.h", two)], "/p/main.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FLAG;"));

        let Known::Yes(found) = found else {
            panic!("order is the whole answer: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/one.h"));
    }

    #[test]
    fn a_local_define_after_an_include_wins_over_the_header() {
        let source = "#include \"config.h\"\n#define FEATURE 2\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[("/p/config.h", "#define FEATURE 1\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        let Known::Yes(found) = found else {
            panic!("the file's own later define is what is in force: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/main.cpp"));
    }

    #[test]
    fn a_local_define_before_an_include_loses_to_the_header() {
        // The same two facts as the test above with the lines the other way round, which is the whole point: this
        // is a positional query, so it is not enough to know that both files define the name.
        let source = "#define FEATURE 2\n#include \"config.h\"\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[("/p/config.h", "#define FEATURE 1\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        let Known::Yes(found) = found else {
            panic!("the header was pasted after the local define: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/config.h"));
    }

    #[test]
    fn a_nested_include_is_reached() {
        let source = "#include \"middle.h\"\nint x = DEEP;\n";
        let (index, tree) = analysed(
            &[
                ("/p/middle.h", "#include \"leaf.h\"\n"),
                ("/p/leaf.h", "#define DEEP 1\n"),
            ],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "DEEP;"));

        let Known::Yes(found) = found else {
            panic!("a define two includes down is still in force: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/leaf.h"));
    }

    #[test]
    fn an_undef_of_a_local_define_leaves_an_ordinary_identifier() {
        // The answer is not the `#define` it used to have: pointing there would send a user to a definition that is
        // not in force, which is a wrong answer rather than a missing one.
        let source = "#define MAX 1\n#undef MAX\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
            "an `#undef` above the use settles it: {found:?}"
        );
    }

    #[test]
    fn an_undef_of_a_headers_define_also_counts() {
        // Across a file boundary, which is the case a single-file macro table cannot see at all: the `#undef` is
        // in the querying file and the `#define` is in the header.
        let source = "#include \"config.h\"\n#undef FEATURE\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[("/p/config.h", "#define FEATURE 1\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
            "the header's define is ended by the file's own `#undef`: {found:?}"
        );
    }

    #[test]
    fn a_define_after_an_undef_is_in_force_again() {
        let source = "#define MAX 1\n#undef MAX\n#define MAX 2\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
        let Known::Yes(found) = found else {
            panic!("the last fact is a define: {found:?}");
        };
        assert_eq!(line_of(source, found.fact.range.start_offset), 2);
    }

    #[test]
    fn a_define_inside_a_conditional_is_unknown_rather_than_guessed() {
        let source = "#if defined(USE_MAX)\n#define MAX 1\n#endif\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
            "whether that branch was taken is not knowable here: {found:?}"
        );
    }

    #[test]
    fn an_unconditional_define_beats_a_guarded_one_after_it() {
        // The rule the declaration query uses, applied to positions: the name certainly is a macro, so refusing to
        // answer would be refusing a question that has an answer. What the guarded define might do is stated in the
        // documentation rather than turned into an `Unknown`.
        let source = "#define MAX 1\n#if defined(OTHER)\n#define MAX 3\n#endif\nint x = MAX;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
        let Known::Yes(found) = found else {
            panic!("the unconditional define is what is certain: {found:?}");
        };
        assert_eq!(line_of(source, found.fact.range.start_offset), 0);
    }

    #[test]
    fn a_guarded_include_makes_the_answer_conditional() {
        let source = "#if defined(USE_CONFIG)\n#include \"config.h\"\n#endif\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[("/p/config.h", "#define FEATURE 1\n")],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
            "the only path to the define goes through an `#if`: {found:?}"
        );
    }

    #[test]
    fn a_name_no_one_defines_is_not_declared_here() {
        let source = "int x = NOT_A_MACRO;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "NOT_A_MACRO;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "the index is a subset of the translation unit, so this is not a definite no: {found:?}"
        );
    }

    #[test]
    fn a_cursor_that_is_not_on_a_name_has_nothing_to_look_up() {
        let source = "#define MAX 1\nint x = MAX + 1;\n";
        let (index, tree) = analysed(&[], "/p/a.cpp", source);
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "+ 1;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "the cursor is on an operator: {found:?}"
        );
    }

    #[test]
    fn a_cycle_of_includes_terminates() {
        // Two headers that include each other, both defining the name: the walk expands a file once per query, so
        // it ends instead of growing a chain for ever.
        let source = "#include \"a.h\"\nint x = SHARED;\n";
        let (index, tree) = analysed(
            &[
                ("/p/a.h", "#include \"b.h\"\n#define SHARED 1\n"),
                ("/p/b.h", "#include \"a.h\"\n"),
            ],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "SHARED;"));
        let Known::Yes(found) = found else {
            panic!("a cycle must not stop the define being found: {found:?}");
        };
        assert_eq!(found.file, Path::new("/p/a.h"));
    }

    #[test]
    fn an_undef_in_a_header_is_reached_like_a_define() {
        // The symmetric case, and the reason `#undef` is a fact rather than a note: the fact that settles the
        // question can be in either file.
        let source = "#include \"config.h\"\n#include \"cleanup.h\"\nint x = FEATURE;\n";
        let (index, tree) = analysed(
            &[
                ("/p/config.h", "#define FEATURE 1\n"),
                ("/p/cleanup.h", "#undef FEATURE\n"),
            ],
            "/p/main.cpp",
            source,
        );
        let root = tree.get_red_root();

        let found = super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FEATURE;"));
        assert!(
            matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
            "the later header undefines it: {found:?}"
        );
    }
}




/// The name index against the scan it replaced: the same questions, asked of the same index, answered both ways.
///
/// The inverted index is derived data, and derived data has one failure that no ordinary test finds — it drifts
/// from what it was derived from after some particular sequence of edits. So this builds a corpus that has every
/// kind of collision (one name in many files, one name in many scopes, a name only the cooked reading declares),
/// mutates it the way a session does (forget, re-insert edited, cook, forget the cooking), and after **every**
/// step asserts that each name query equals the brute-force answer over `summaries()`.
#[cfg(test)]
mod name_index_agrees_with_the_scan {
    use super::*;
    use crate::cache::SummaryKey;
    use crate::index::summarize;

    const WORDS: [&str; 6] = ["Widget", "size", "run", "Gadget", "count", "widest"];

    /// A small deterministic generator: the corpus has to be the same on every machine and every run.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, below: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % below
        }
    }

    fn source_of(number: usize, generator: &mut Lcg) -> String {
        let mut text = String::new();
        for _ in 0..generator.next(3) {
            text.push_str(&format!("#include \"f{}.h\"\n", generator.next(number.max(1))));
        }
        text.push_str(&format!("#define DECL{number}(n) struct n {{ int v; }};\n"));
        text.push_str(&format!("DECL{number}(Made{})\n", generator.next(4)));
        text.push_str(&format!("namespace ns{} {{\n", generator.next(3)));
        for _ in 0..1 + generator.next(3) {
            let class = WORDS[generator.next(WORDS.len())];
            let member = WORDS[generator.next(WORDS.len())];
            text.push_str(&format!("struct {class} {{ int {member}; void {member}2(); }};\n"));
        }
        text.push_str("}\n");
        text.push_str(&format!("int {};\n", WORDS[generator.next(WORDS.len())]));
        text.push_str(&format!("void fn{number}() {{ int {}; }}\n", WORDS[generator.next(WORDS.len())]));
        text
    }

    fn file(index: &mut ProjectIndex, number: usize, source: &str) {
        let path = format!("/p/f{number}.h");
        let mut summary = summarize(Path::new(&path), source, SummaryKey::new(number as u64, 0));
        for include in &mut summary.includes {
            include.resolved = Some(PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(summary);
    }

    fn cook(index: &mut ProjectIndex, number: usize, source: &str) {
        let path = format!("/p/f{number}.h");
        let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
        let rendered = crate::preprocess::cooked::cook(source, &tokens).render();
        let provider = crate::MemoryFiles::new();
        let config = crate::CompilerConfig::default();
        let indexed = crate::FileIndexer::new(&provider, &config).index_rendering(
            Path::new(&path),
            &rendered,
            SummaryKey::new(number as u64, 0),
        );
        index.insert_cooked(Path::new(&path), indexed.into());
    }

    type Row = (PathBuf, String, Option<String>, usize, IncludeVisibility);

    fn row(file: &Path, fact: &DeclFact, visibility: IncludeVisibility) -> Row {
        (file.to_path_buf(), fact.name.clone(), fact.scope.clone(), fact.range.start_offset, visibility)
    }

    /// Every fact a query can see, with the raw-then-cooked deduplication, the way the per-file scan did it.
    fn scanned(
        index: &ProjectIndex,
        from: &Path,
        accepts: impl Fn(&DeclFact) -> bool,
    ) -> Vec<Row> {
        let visible: HashMap<String, IncludeVisibility> = index.visible_files(from).into_iter().collect();
        let mut rows = Vec::new();

        for summary in index.summaries() {
            let Some(visibility) = visible.get(&normalize(&summary.path)).copied() else {
                continue;
            };
            let raw: Vec<&DeclFact> = summary.declarations.iter().filter(|fact| accepts(fact)).collect();
            rows.extend(raw.iter().map(|fact| row(&summary.path, fact, visibility)));

            for fact in index
                .cooked_declarations(&summary.path)
                .into_iter()
                .flatten()
                .filter(|fact| accepts(fact))
            {
                if !raw.iter().any(|known| known.name == fact.name && known.kind == fact.kind) {
                    rows.push(row(&summary.path, fact, visibility));
                }
            }
        }

        rows
    }

    fn rows(found: &[VisibleDeclaration<'_>]) -> Vec<Row> {
        found.iter().map(|found| row(&found.file, found.fact, found.visibility)).collect()
    }

    /// The workspace search as it was written before the index: every declaration of every file.
    fn symbols_by_scanning(index: &ProjectIndex, query: &str, limit: usize) -> Vec<(PathBuf, String, Option<String>, usize)> {
        let wanted = query.trim().to_lowercase();
        let mut matches: Vec<(usize, String, String, PathBuf, &DeclFact)> = Vec::new();

        for summary in index.summaries() {
            let raw: Vec<&DeclFact> = summary.declarations.iter().filter(|fact| !fact.local).collect();
            let fresh: Vec<&DeclFact> = index
                .cooked_declarations(&summary.path)
                .into_iter()
                .flatten()
                .filter(|fact| {
                    !fact.local && !raw.iter().any(|known| known.name == fact.name && known.kind == fact.kind)
                })
                .collect();

            for fact in raw.into_iter().chain(fresh) {
                let qualified = fact.qualified_name();
                let Some(rank) = rank_of(&qualified, &fact.name, &wanted) else {
                    continue;
                };
                let qualified = qualified.to_lowercase();
                let name = match fact.name.is_empty() {
                    false => fact.name.to_lowercase(),
                    true => qualified.rsplit("::").next().unwrap_or_default().to_string(),
                };
                matches.push((rank, name, qualified, summary.path.clone(), fact));
            }
        }

        matches.sort_by(|one, two| {
            one.0
                .cmp(&two.0)
                .then_with(|| one.1.cmp(&two.1))
                .then_with(|| one.2.cmp(&two.2))
                .then_with(|| one.3.cmp(&two.3))
        });
        matches
            .into_iter()
            .take(limit)
            .map(|(_, _, _, file, fact)| (file, fact.name.clone(), fact.scope.clone(), fact.range.start_offset))
            .collect()
    }

    fn check(index: &ProjectIndex, files: usize, step: &str) {
        let asked_from: Vec<PathBuf> = (0..files).step_by(3).map(|n| PathBuf::from(format!("/p/f{n}.h"))).collect();

        for from in &asked_from {
            for word in WORDS.iter().copied().chain(["Made0", "Made3", "nothing", "ns0::Widget", "::size", "ns1::Gadget"]) {
                assert_eq!(
                    rows(&index.files_declaring(word, from)),
                    scanned(index, from, |fact| matches(fact, word)),
                    "{step}: files_declaring({word:?}) from {from:?}"
                );
            }
            for scope in ["ns0", "ns1", "ns2", "ns0::Widget", "ns1::Gadget", "ns2::size", "nowhere"] {
                assert_eq!(
                    rows(&index.declarations_in(scope, from)),
                    scanned(index, from, |fact| fact.scope.as_deref() == Some(scope)),
                    "{step}: declarations_in({scope:?}) from {from:?}"
                );
            }
        }

        for query in [
            "w", "wid", "Widget", "size", "ma", "made", "run2", "ns0::wid", "ns1::Gadget::si", "Widget::", "::widget", "ns0::",
            "zzz", "  count ", "e",
        ] {
            for limit in [1, 4, 1000] {
                let found: Vec<_> = index
                    .symbols_matching(query, limit)
                    .into_iter()
                    .map(|symbol| (symbol.file, symbol.fact.name, symbol.fact.scope, symbol.fact.range.start_offset))
                    .collect();
                assert_eq!(found, symbols_by_scanning(index, query, limit), "{step}: symbols_matching({query:?}, {limit})");
            }
        }

        let defining: Vec<PathBuf> = index
            .summaries()
            .filter(|summary| summary.macros.iter().any(|fact| fact.name == "DECL3" && fact.kind.is_definition()))
            .map(|summary| summary.path.clone())
            .collect();
        assert_eq!(index.files_defining_macro("DECL3"), defining, "{step}: files_defining_macro");
    }

    /// A name declared thousands of times is cut at the limit rather than ordered — and the cut is deterministic.
    #[test]
    fn a_name_declared_everywhere_answers_the_first_hits_in_file_order() {
        let mut index = ProjectIndex::new();
        for number in 0..POPULAR_NAME + 50 {
            file(&mut index, number, &format!("struct C{number} {{ void run(); }};\n"));
        }

        let found = index.symbols_matching("run", 10);
        assert_eq!(found.len(), 10);
        assert_eq!(found, index.symbols_matching("run", 10), "the same answer twice");
        assert!(
            found.iter().all(|symbol| {
                let number: usize = symbol.file.to_string_lossy()[4..].trim_end_matches(".h").parse().unwrap();
                number < 10
            }),
            "the cut takes the files in the order the index saw them: {found:?}"
        );
    }

    #[test]
    fn after_every_kind_of_edit() {
        const FILES: usize = 24;
        let mut generator = Lcg(7);
        let mut sources: Vec<String> = Vec::new();
        let mut index = ProjectIndex::new();

        for number in 0..FILES {
            sources.push(source_of(number, &mut generator));
            file(&mut index, number, &sources[number]);
        }
        check(&index, FILES, "after building");

        for number in (0..FILES).step_by(2) {
            cook(&mut index, number, &sources[number]);
        }
        check(&index, FILES, "after cooking half of them");
        // The corpus is not vacuous: the collisions the checks are about are really there.
        assert!(index.symbols_matching("widget", 1000).len() > 10, "one name, many files and scopes");
        assert!(
            index.symbols_matching("made", 1000).iter().any(|symbol| symbol.fact.name.starts_with("Made")),
            "a name only the cooked reading declares"
        );
        assert!(!index.declarations_in("ns0", Path::new("/p/f21.h")).is_empty());

        // An edit, as a session does one: the summary is replaced under the same key and the reading is dropped.
        for number in [3, 4, 11] {
            index.forget(Path::new(&format!("/p/f{number}.h")));
            sources[number] = source_of(number + 100, &mut generator);
            file(&mut index, number, &sources[number]);
        }
        check(&index, FILES, "after forgetting and re-reading three files");

        // A replacement under a *different* key (the key is made from the text) drops the reading with it.
        let path = Path::new("/p/f6.h");
        sources[6].push_str("int rekeyed;
");
        let mut summary = summarize(path, &sources[6], SummaryKey::new(999, 0));
        for include in &mut summary.includes {
            include.resolved = Some(PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(summary);
        assert!(index.cooked_declarations(path).is_none(), "the reading described a different key");
        check(&index, FILES, "after a re-key");

        for number in [0, 2, 8] {
            index.forget_cooked(Path::new(&format!("/p/f{number}.h")));
        }
        check(&index, FILES, "after forgetting three readings");

        cook(&mut index, 8, &sources[8]);
        cook(&mut index, 8, &sources[8]);
        check(&index, FILES, "after cooking one twice");

        for number in 0..FILES {
            index.forget(Path::new(&format!("/p/f{number}.h")));
        }
        assert_eq!(index.distinct_names(), 0, "an index with no files has no names");
        assert!(index.symbols_matching("w", 10).is_empty());
    }
}
