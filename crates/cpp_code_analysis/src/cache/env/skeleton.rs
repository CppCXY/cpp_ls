//! **A file's directives, in the order a compiler reads them** — the input a fold needs, and nothing else.
//!
//! The fold in [`super::compose`] does not read a file's text. Everything it needs is already in the file's summary:
//! its `#define`s and `#undef`s (`MacroFact`), its `#include`s (`IncludeFact`, with the path the resolver found), and
//! the region each of them was written in (`FactGuard`). What this module adds is **order** — summaries store facts by
//! kind, and a fold has to see them interleaved — and the one thing a summary does not carry: the **body text** of a
//! definition, sliced out of the file it was written in.
//!
//! # Why the body text is sliced here and not stored in the fact
//!
//! A `MacroFact` stores a body as a [`cpp_parser::MacroBody`] — a *shape* ("a specifier", "a statement") — plus
//! `value` and `alias`, the two pieces a condition can read. That is the right size for a summary (486 783
//! declarations and 149 164 macro facts), and it is enough for every query that asks "what does this macro mean" by
//! looking at the file. An environment is asked a different question — "what would this expansion paste" — and the
//! answer is the bytes, which are in `body_range`. Slicing them costs one copy per definition the fold applies, and
//! the environment keeps them for as long as it is asked.
//!
//! # Which include path a step uses
//!
//! [`Skeleton::of`] takes the one the summary cached (`IncludeFact::resolved`). [`Skeleton::resolved_by`] takes a
//! **resolver** instead, and that is the shape a real project needs: the cached path is what the index found when the
//! summary was built, and on a project with no compile database it is empty for most includes — measured on
//! `EmmyLuaCodeStyle`, **778 of the 1 053-file closure's includes**, so a fold that trusts it reaches 251 files where
//! the walk reaches 1 053. A caller that can search again (see `Session`'s resolver, which is what the walk itself
//! uses) passes the search here, and a resolver that only handles the unresolved ones costs one branch per include.
//!
//! # What is not a step
//!
//! * An include whose target did not resolve. The resolver already decided there is no such file (§"a summary that
//!   records a failed search must not be stored"), and a fold cannot recurse into a file the analysis cannot find.
//!   Counted in [`Skeleton::unresolved_includes`], so a probe can say how much of a corpus it did not compose.
//! * A fact whose `body_range` does not fit its text. Counted in [`Skeleton::unreadable`]. A summary and a text that
//!   disagree are a bug elsewhere, and guessing at the definition would hide it.

use std::path::PathBuf;
use std::sync::Arc;

use crate::FactGuard;
use crate::cache::summary::{FileSummary, MacroKind};

use super::Definition;

/// **One directive a fold must consider** — where it is, what it is under, and what it does.
#[derive(Debug, Clone)]
pub struct Step {
    /// The directive's offset in the file, which is what orders the steps.
    pub at: usize,
    /// The region it was written in; the fold decides whether that region is in force.
    pub guard: FactGuard,
    pub action: Action,
}

#[derive(Debug, Clone)]
pub enum Action {
    Define(Arc<Definition>),
    Undef(Arc<str>),
    Include(IncludeAt),
}

/// An include, with the file the resolver found for it.
#[derive(Debug, Clone)]
pub struct IncludeAt {
    pub path: PathBuf,
}

/// **A file's directives in source order.**
#[derive(Debug, Clone, Default)]
pub struct Skeleton {
    steps: Vec<Step>,
    unresolved_includes: usize,
    unreadable: usize,
}

impl Skeleton {
    /// **Read a summary and a text into the steps a fold takes**, using the resolution the summary cached.
    ///
    /// The two kinds of step are gathered separately (that is how the summary stores them) and then merged by
    /// offset. The sort is stable and the offsets are distinct for distinct directives, so two skeletons of one file
    /// are equal — which is what lets a probe compare two compositions without comparing `Step`s.
    pub fn of(summary: &FileSummary, text: &str) -> Self {
        Self::resolved_by(summary, text, &mut |fact| fact.resolved.clone())
    }

    /// **The same, with a resolver instead of the summary's cached answer** — see the module note.
    ///
    /// The resolver is asked for **every** include, not only the unresolved ones, because a cached path is a
    /// candidate the index verified when the summary was built: if the search now says something else, the search is
    /// right. Returning `None` drops the include as unresolved.
    pub fn resolved_by(
        summary: &FileSummary,
        text: &str,
        resolve: &mut dyn FnMut(&crate::IncludeFact) -> Option<PathBuf>,
    ) -> Self {
        let mut steps: Vec<Step> =
            Vec::with_capacity(summary.macros.len() + summary.includes.len());
        let mut unreadable = 0usize;
        for fact in &summary.macros {
            match Definition::of(fact, text) {
                Some(definition) => steps.push(Step {
                    at: fact.range.start_offset,
                    guard: fact.guard,
                    action: Action::Define(Arc::new(definition)),
                }),
                None if fact.kind == MacroKind::Undefinition => steps.push(Step {
                    at: fact.range.start_offset,
                    guard: fact.guard,
                    action: Action::Undef(Arc::from(fact.name.as_str())),
                }),
                // A definition whose body cannot be sliced: see the module note.
                None => unreadable += 1,
            }
        }
        let mut unresolved_includes = 0usize;
        for fact in &summary.includes {
            let Some(path) = resolve(fact) else {
                unresolved_includes += 1;
                continue;
            };
            steps.push(Step {
                at: fact.range.start_offset,
                guard: fact.guard,
                action: Action::Include(IncludeAt { path }),
            });
        }
        steps.sort_by_key(|step| step.at);
        Skeleton {
            steps,
            unresolved_includes,
            unreadable,
        }
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Includes whose target the resolver could not find — the part of a corpus a composition cannot see.
    pub fn unresolved_includes(&self) -> usize {
        self.unresolved_includes
    }

    /// Definitions whose body range does not fit the text they came with.
    pub fn unreadable(&self) -> usize {
        self.unreadable
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}
