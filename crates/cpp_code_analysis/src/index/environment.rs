//! The macros a **query** evaluates a file's conditions against.
//!
//! A summary stores each conditional region as the question it asks ([`crate::SummaryGuards`]), and the answer
//! depends on the compilation: `-D`s, the `-std=` that fixes `__cplusplus`, and the five hundred names a compiler
//! predefines. None of that is in a summary, and none of it can be: a summary's key is computed from the text and
//! the paths alone, which is what lets the cache be consulted before anything is parsed. So the environment
//! arrives at the query, is applied to the stored question, and the answer is thrown away with the query.
//!
//! # What this is complete about, and what it is not
//!
//! Three sources of definitions are known here, and they are the three a query can defend:
//!
//! ```text
//! the compiler's predefined names   asked once, with `-dM` (see `Toolchain::macros`)
//! the command line's `-D`/`-U`      read from the compile database's entry
//! this file's own `#define`s        the summary's macro facts, above the point being asked about
//! ```
//!
//! Everything a *header* defines is missing, and that absence is the whole design: a name none of the three
//! mentions is [`Lookup::Unanswered`], which the evaluator turns into `Unknown` rather than into the standard's
//! `0`. The alternative — reading "no file I indexed defines it" as "it is not defined" — is how
//! `#ifdef NT_INCLUDED` comes out false in a header that a real translation unit includes, and a wrong answer
//! there is worse than a missing one: it hides declarations that exist.
//!
//! The next step is recorded in `docs/roadmap.md`: the macros a *closure* defines, threaded in translation order
//! by the same walk that finds the files. That is the half which would decide the conditions naming a header's own
//! feature macros (`_GLIBCXX_USE_CXX11_ABI` and the like), and it needs the complete-input question answered
//! first — an unresolved `#include` or an unindexed header is a hole in exactly the reasoning that would make
//! "not defined" sound.

use std::path::Path;

use crate::condition::{Lookup, MacroValues};
use crate::guard::Visibility;
use crate::include::graph::Marked;
use crate::summary::{FactGuard, FileSummary, MacroFact, MacroKind};
use crate::ProjectIndex;

/// The macros in force at one point in one file, as far as a query can know.
///
/// Cheap to build — two references and an offset — because one is built per condition: the question "what does
/// this `#if` mean" is asked where the condition was written, which is a different offset for every region.
pub struct MacrosHere<'a> {
    /// What the compilation starts with, plus — for the walk's own caller — every fact the walk has passed.
    /// Deliberately an **incomplete** [`Marked`], so that a name it does not mention is unanswered rather than
    /// undefined.
    ///
    /// Borrowed when a walk already has one, owned when it was built for this one condition
    /// ([`MacrosHere::from_summary`] asks [`ProjectIndex::macros_at`], which walks): a condition has at most a few
    /// names to look up, and borrowing the walk's state is the difference between one traversal and one per
    /// condition.
    seed: std::borrow::Cow<'a, Marked>,
    /// The file the condition is written in, for what it does to a name above that point — `None` when the state
    /// is already up to date, which is what the walk passes: it has applied the file's own facts, with their
    /// values, and looking them up again here would answer with less than the state knows.
    file: Option<&'a FileSummary>,
    /// The offset the file's own facts are read up to: the condition's own position.
    offset: usize,
}

impl<'a> MacrosHere<'a> {
    /// The macros at a condition a **walk** has reached, whose state already includes the facts above it.
    pub fn from_walk(state: &'a Marked, offset: usize) -> Self {
        MacrosHere {
            seed: std::borrow::Cow::Borrowed(state),
            file: None,
            offset,
        }
    }

    /// The macros at a condition in `file`, given only an environment: the file's own facts above the point are
    /// read from the summary.
    ///
    /// The state a query uses comes from a walk; this is for a caller that has an offset and an index and no walk
    /// — [`visibility_at`], which is how a probe or a test asks the same question.
    pub fn from_summary(state: Marked, file: &'a FileSummary, offset: usize) -> Self {
        MacrosHere {
            seed: std::borrow::Cow::Owned(state),
            file: Some(file),
            offset,
        }
    }
}

impl MacroValues for MacrosHere<'_> {
    fn lookup(&self, name: &str) -> Lookup<'_> {
        // The file's own last word above the point wins, because that is the order a preprocessor reads in: a
        // `#define` here shadows the command line, and `#undef __cplusplus` — which real test suites write —
        // really does remove a compiler's built-in.
        if let Some(fact) = self.last_word_about(name) {
            return match fact.kind {
                MacroKind::Definition => Lookup::DefinedWithoutAValue,
                MacroKind::Undefinition => Lookup::Undefined,
            };
        }

        // What the compiler and the command line define, and `Unanswered` for anything they do not mention: the
        // headers that could define it have not been walked *for this question*, and pretending otherwise is the
        // one move this layer must not make.
        self.seed.as_ref().lookup(name)
    }
}

impl<'a> MacrosHere<'a> {
    /// The file's own last fact about `name` at or above `offset`.
    ///
    /// Only facts **outside every conditional** count. A `#define FOO` written inside an `#if` may not be there
    /// at all, and treating it as in force would decide the very condition that is being asked about — with
    /// evidence that depends on the answer.
    ///
    /// A summary holds tens to hundreds of macro facts, and a condition mentions a handful of names, so this is a
    /// scan rather than an index. The alternative — a name-keyed map built for every condition — allocates once
    /// per region per query to save a walk over a list that fits in a cache line.
    fn last_word_about(&self, name: &str) -> Option<&'a MacroFact> {
        self.file?
            .macros
            .iter()
            .filter(|fact| {
                &*fact.name == name
                    && fact.guard == FactGuard::Unconditional
                    && fact.range.start_offset <= self.offset
            })
            .max_by_key(|fact| fact.range.start_offset)
    }
}

/// Was the code the guard names compiled, as far as this index can tell?
///
/// [`Visibility::Active`] — every enclosing condition is decided, and taken (or there is no condition at all,
/// which is decided without evaluating anything). [`Visibility::Inactive`] — one of them is decided and *not*
/// taken, so a compiler would not have read this code at all. [`Visibility::Unknown`] — at least one condition
/// cannot be decided from what this index knows, which is the honest answer for most `#if`s in most projects
/// today.
///
/// Takes the guard rather than a bare offset because every caller has one: a fact records the region it was
/// written in, and the region is where the walk outwards starts. See [`crate::SummaryGuards::visibility_of`].
///
/// # Why this builds a state per condition
///
/// Each condition is evaluated against the macros **at its own offset**, which is a walk
/// ([`ProjectIndex::macros_at`]) rather than the environment the index was seeded with. Two reasons, and both are
/// the difference between an answer and a wrong one:
///
/// * a `#define` written above the condition is in force and one written below is not — for every condition in the
///   file, including the ones about a *name the same file defines*;
/// * a condition must be read **before** its own body, or `#ifndef NAME / #define NAME` decides itself backwards.
///
/// A caller already walking the graph — [`ProjectIndex::macro_environment`] — has the state in hand and uses
/// [`crate::index::environment::MacrosHere::from_walk`] instead: one traversal, not one per condition.
pub fn visibility_at(
    index: &ProjectIndex,
    path: &Path,
    guard: FactGuard,
    offset: usize,
) -> Visibility {
    if guard == FactGuard::Unconditional {
        return Visibility::Active;
    }

    let Some(summary) = index.summary(path) else {
        return Visibility::Unknown;
    };

    summary.guards.visibility_of(guard, offset, |condition_at| {
        MacrosHere::from_summary(index.macros_at(path, condition_at), summary, condition_at)
    })
}

#[cfg(test)]
mod tests {
    use super::visibility_at;
    use crate::cache::SummaryKey;
    use crate::guard::Visibility;
    use crate::index::ProjectIndex;
    use crate::summary::{FactGuard, SummaryGuards};
    use std::path::Path;

    /// An index over one file, with whatever environment the test names.
    ///
    /// **Incomplete**, which is what a fixture with no project around it is: nothing has said that these are all
    /// the compilation's definitions, so a name the environment does not mention may still be defined by a header
    /// — and `#ifdef` on it is `Unknown` rather than false. A real session without a compile database is in the
    /// same position (see `compilation_environment` in `session.rs`), and
    /// [`a_name_nothing_defines_is_not_defined_when_the_inputs_are_complete`] is the other side of it.
    fn index_of(source: &str, defines: &[&str]) -> ProjectIndex {
        index_of_with(source, defines, false)
    }

    /// The same, told whether the environment is the whole of what the compilation defines.
    fn index_of_with(source: &str, defines: &[&str], complete: bool) -> ProjectIndex {
        let path = Path::new("/p/widget.h");
        let summary = crate::index::summarize(path, source, SummaryKey::new(0, 0));

        let mut macros = crate::Marked::default();
        for name in defines {
            macros.define_on_the_command_line(name, None);
        }

        let mut index = ProjectIndex::new().with_macros(if complete {
            macros
        } else {
            macros.incomplete()
        });
        index.insert(summary);
        index
    }

    /// The guard of the fact naming `needle`, and where it is.
    fn guard_of(source: &str, defines: &[&str], needle: &str) -> (ProjectIndex, FactGuard, usize) {
        let index = index_of(source, defines);
        let (guard, offset) = guard_in(&index, source, needle);

        (index, guard, offset)
    }

    /// The same, for an index a test built itself — so that it can choose the completeness on its own.
    fn guard_in(index: &ProjectIndex, source: &str, needle: &str) -> (FactGuard, usize) {
        let offset = source.find(needle).expect("the needle is in the fixture");
        let guard = index
            .summary(Path::new("/p/widget.h"))
            .expect("the file is indexed")
            .macros
            .iter()
            .find(|fact| fact.range.start_offset == offset)
            .map(|fact| fact.guard)
            .expect("a fact starts at the needle");

        (guard, offset)
    }

    /// [`guard_of`] with an environment the caller declares **complete**.
    fn guard_of_in_complete(
        source: &str,
        defines: &[&str],
        needle: &str,
    ) -> (ProjectIndex, FactGuard, usize) {
        let index = index_of_with(source, defines, true);
        let (guard, offset) = guard_in(&index, source, needle);

        (index, guard, offset)
    }

    #[test]
    fn a_name_nothing_defines_is_not_defined_when_the_inputs_are_complete() {
        // The other side of the coin, and the claim [`crate::Session::open`] makes when it read the project's own
        // compile database: these are the compilation's definitions, every `#include` that mattered resolved, and
        // nothing indexed defines `NT_INCLUDED` — so the guard *is* taken.
        //
        // This is the decision the whole macro line was blocked on (`docs/std-library.md`, round 14): `windef.h`
        // has `#ifndef NT_INCLUDED / #include <winnt.h> / #endif`, `NT_INCLUDED` is defined nowhere in the MinGW
        // headers, and every one of `STDMETHODCALLTYPE`'s four thousand references was "maybe" because of it.
        let source = "int early;\n#ifndef NT_INCLUDED\n#include \"winnt.h\"\n#endif\n";
        let path = Path::new("/p/widget.h");
        let (guard, offset) = include_guard(&index_of_with(source, &[], true));

        assert_eq!(
            visibility_at(&index_of_with(source, &[], true), path, guard, offset),
            Visibility::Active,
            "nothing defines it, and the caller said nothing else can: the include is certain"
        );

        // …and the same file with an incomplete environment is `Unknown`, which is the answer that costs an answer
        // rather than giving a wrong one.
        assert_eq!(
            visibility_at(&index_of(source, &[]), path, guard, offset),
            Visibility::Unknown
        );
    }

    /// The include's own guard and offset — the position a condition about an *include* is asked at.
    fn include_guard(index: &ProjectIndex) -> (FactGuard, usize) {
        let summary = index
            .summary(Path::new("/p/widget.h"))
            .expect("the file is indexed");
        let include = summary.includes.first().expect("the fixture includes a file");

        (include.guard, include.range.start_offset)
    }

    #[test]
    fn the_files_own_guard_is_transparent_to_everything_inside_it() {
        // A header is `#ifndef GUARD / #define GUARD` and then a file full of conditions. The guard is not one of
        // them: entering the file at all is what it means, so a fact nested inside it must be decided by the
        // conditions *it* is written in — not by a guard that has, by that point, defined its own name and would
        // read as "not taken" if it were evaluated like any other `#ifndef`.
        //
        // This is the shape that made `_GLIBCXX_USE_CXX11_ABI` come out `0` in the standard library (round 15):
        // `bits/c++config.h` is guarded, and its `#define _GLIBCXX_USE_CXX11_ABI 1` sits inside `#if __cplusplus`
        // inside that guard.
        let source = "#ifndef WIDGET_H\n#define WIDGET_H\n#if __cplusplus\n#define INNER 1\n#endif\n#endif\n";
        let path = Path::new("/p/widget.h");
        let (index, guard, offset) = guard_of_in_complete(source, &["__cplusplus"], "INNER");

        assert_eq!(
            visibility_at(&index, path, guard, offset),
            Visibility::Active,
            "the condition is `#if __cplusplus`, and the guard around it is not a condition at all"
        );

        // And the guard is still *recorded*: it is the region the file's own guard opens, which is what makes the
        // rule above applicable rather than a blanket "the outermost region does not count".
        let summary = index.summary(path).expect("the file is indexed");
        assert_eq!(summary.guards.own_guard, Some(0));
        assert!(
            summary.guards.conditions_of(0).is_empty(),
            "and asking about the guard itself gets no condition back"
        );
    }

    #[test]
    fn code_outside_every_conditional_is_active_without_being_evaluated() {
        // The fast path, which is also the common case: an unconditional include is one no query has to think
        // about, and an index that has been told nothing must still answer this — the region search would find no
        // condition to evaluate, and "no condition" is not "cannot tell".
        let index = index_of("#define NAME 1\n", &[]);
        let path = Path::new("/p/widget.h");

        assert_eq!(
            visibility_at(&index, path, FactGuard::Unconditional, 0),
            Visibility::Active
        );
        assert!(
            index.macros().is_empty(),
            "and it did not need an environment to say so"
        );
    }

    #[test]
    fn a_condition_the_environment_decides_is_decided() {
        // The three answers, from the same file and the same condition, with three environments. `X` is a fact
        // about the compilation, so the *file* is the same in all three.
        let source = "#ifdef X\n#define INNER 1\n#endif\n";
        let path = Path::new("/p/widget.h");

        let (index, guard, offset) = guard_of(source, &["X"], "INNER");
        assert_eq!(visibility_at(&index, path, guard, offset), Visibility::Active);

        let (index, guard, offset) = guard_of(source, &[], "INNER");
        assert_eq!(
            visibility_at(&index, path, guard, offset),
            Visibility::Unknown,
            "a name nothing the index read mentions is not 'not defined'"
        );

        // The same question with the *other* environment answer, which is the caller's to give: an environment
        // that is the whole of what the compilation defines makes the guard not taken instead of unknown.
        let (index, guard, offset) = guard_of_in_complete(source, &[], "INNER");
        assert_eq!(
            visibility_at(&index, path, guard, offset),
            Visibility::Inactive,
            "nothing defines `X`, and the caller said nothing else can"
        );

        // And the decided *no*: `#if 0` asks nothing, so nothing can be missing.
        let source = "#if 0\n#define INNER 1\n#endif\n";
        let (index, guard, offset) = guard_of(source, &[], "INNER");
        assert_eq!(
            visibility_at(&index, path, guard, offset),
            Visibility::Inactive
        );
    }

    #[test]
    fn a_region_the_summary_does_not_describe_is_unknown_rather_than_taken() {
        // A guard naming a region whose condition is not in the summary: a summary written before the regions
        // carried their conditions, or one whose bytes came from somewhere else. Nothing can be said about it, and
        // the tempting answer — "there is no condition, so it is compiled" — is the one that would silently stop
        // qualifying a fact that may well be behind an `#if`.
        let source = "#ifdef X\n#define INNER 1\n#endif\n";
        let mut index = index_of(source, &["X"]);
        let path = Path::new("/p/widget.h");

        let summary = index.summary(path).expect("the file is indexed");
        let mut stripped = summary.clone();
        stripped.guards = SummaryGuards {
            regions: summary.guards.regions.clone(),
            conditionals: Vec::new(),
            ..SummaryGuards::default()
        };
        index.insert(stripped);

        assert_eq!(
            visibility_at(&index, path, FactGuard::Region(0), 0),
            Visibility::Unknown
        );
    }

    #[test]
    fn a_name_the_file_defines_inside_the_region_is_not_evidence_about_the_region() {
        // A `#define` written *inside* an `#if` may not be there at all, so it is not evidence about the very
        // condition that guards it. The file's **own** include guard is the exception, and it is not an exception
        // made here: the walk de-guards it (reaching the file at all is what a guard means), so no condition is
        // asked about — see `deguard_the_files_own_guard`. The declaration at the top is what keeps this region
        // from being that guard.
        let source = "int early;\n#ifndef WRAPPER\n#define WRAPPER 1\n#define INNER 2\n#endif\n";
        let (index, guard, offset) = guard_of(source, &[], "INNER");

        assert_eq!(
            visibility_at(&index, Path::new("/p/widget.h"), guard, offset),
            Visibility::Unknown,
            "nothing outside the region says whether `WRAPPER` was defined when the `#ifndef` was read"
        );

        let (index, guard, offset) = guard_of(source, &["WRAPPER"], "INNER");
        assert_eq!(
            visibility_at(&index, Path::new("/p/widget.h"), guard, offset),
            Visibility::Inactive,
            "with WRAPPER defined the guard is not taken, and the body is not in the translation unit"
        );
    }
}
