//! **Folding a file's directives into an environment**, and recursing through its includes.
//!
//! This is the replacement for `TranslationUnit::walk` on the *environment* path. The walk scans every file's text
//! and records one event per macro directive; the fold reads the same information from the summaries — which the index
//! already holds — and composes the states a file passes through. Nothing here lexes a file.
//!
//! # What it produces, and why the includes are recorded
//!
//! ```text
//! final_env             the state at the end of the file (what the includer continues from)
//! after_each_include    (offset, state) for every include that was taken — the states a nested query asks about
//! unknown_guards        directives whose region could not be decided (see below)
//! ```
//!
//! # The two shapes of recursion
//!
//! [`compose`] answers "the state at the end of this file", recursing through includes by composing each one. The
//! unit-wide question — "the state at *every* file's entry", which is what a differential test against the walk asks
//! — is the same fold with a different include handler ([`crate::cache::env::compose_unit`]). Both go through
//! [`fold`], so the guard rule, the depth limit and the conservative arm below exist once.
//!
//! # The guard rule, and the one honest gap
//!
//! A directive inside `#if` is applied only when its region is in force, decided with the environment **as of the
//! condition's own offset** — not the fact's offset. The difference is the include-guard shape: `#ifndef X` decides
//! whether the body that writes `#define X` is read at all, so reading the condition at the `#define`'s offset would
//! answer it against a table that already contains the definition. [`crate::SummaryGuards::visibility_of`] asks the
//! caller for a table per condition offset for exactly this reason, and [`Fold::env_at`] is that table.
//!
//! A region that cannot be decided (`Visibility::Unknown`) is **not applied**. That is the conservative direction:
//! applying a definition a compiler would not have read invents a macro, which is the wrong answer that poisons
//! everything downstream; leaving it out can only lose one. The count is in [`Composed::unknown_guards`] so a probe
//! can say how much of a corpus is undecided — with an empty root environment, every `#if SOME_VALUE` is unknown, and
//! with a toolchain seed they are not.
//!
//! # Depth
//!
//! `#include` can be cyclic — an unguarded header including itself is legal text and the preprocessor's own loop,
//! bounded by a nesting limit rather than by a visited set. [`MAX_DEPTH`] is that limit (GCC's 200). A guarded cycle
//! stops earlier on its own: the second entry's `#ifndef` is false, so the body that includes it again is not
//! entered.

use crate::FactGuard;
use std::path::Path;

use crate::Visibility;
use crate::cache::summary::SummaryGuards;
use crate::preprocess::condition::MacroValues;

use super::Over;

use super::{Env, Skeleton, Step, skeleton::Action};

/// The preprocessor's nesting limit, as GCC spells it.
pub const MAX_DEPTH: usize = 200;

/// What composing one file produced.
#[derive(Debug, Clone)]
pub struct Composed {
    /// The environment at the end of the file — what an includer continues from.
    pub final_env: Env,
    /// `(offset, environment)` after each include that was taken, in source order.
    pub after_each_include: Vec<(usize, Env)>,
    /// Directives whose region could not be decided and were therefore not applied.
    pub unknown_guards: usize,
    /// Includes whose target did not resolve (already counted by the skeleton).
    pub unresolved_includes: usize,
    /// Includes that would have nested past [`MAX_DEPTH`].
    pub depth_exceeded: usize,
    /// **The directives whose region could not be decided**, as `(offset, what)` — the attribution a probe needs,
    /// capped so a corpus with thousands of them does not turn a measurement into an allocation. The `what` is the
    /// macro's name or the include's path: what the fold would have applied had the condition been decidable.
    pub unknown: Vec<(usize, String)>,
    /// **Every include the fold skipped**, as `(offset, spelling or path, visibility, condition text)` — an `Inactive`
    /// include hides a whole subtree and adds nothing to [`Composed::unknown_guards`], which is exactly the shape a
    /// traversal can lose most of a closure to without saying so. Capped like [`Composed::unknown`].
    pub skipped_includes: Vec<(usize, String, &'static str, String)>,
}

/// How many undecided directives a fold keeps for a probe — enough to name the classes, few enough that a corpus with
/// thousands of them allocates nothing worth measuring.
pub const UNKNOWN_EXAMPLES: usize = 64;

/// **The condition a directive is under, as text** — the attribution a probe needs: a name says *what* was lost,
/// this says *why*. The region chain is printed outermost first, joined by  && , because a fact is in force only
/// when every enclosing region is.
fn guard_text(guards: &SummaryGuards, guard: FactGuard) -> String {
    let FactGuard::Region(region) = guard else {
        return "unconditional".to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    let mut current = Some(region);
    while let Some(index) = current {
        let Some(conditional) = guards.conditionals.get(index as usize) else {
            break;
        };
        if let Some(branch) = conditional.branches.first() {
            parts.push(format!(
                "{:?} {}",
                branch.kind,
                branch.condition.as_deref().unwrap_or("")
            ));
        }
        current = conditional.parent;
    }
    parts.join(" && ")
}

/// **What a directive is, for a probe's report** — its name, or the path it includes.
fn step_label(step: &Step) -> String {
    match &step.action {
        Action::Define(definition) => definition.name().to_string(),
        Action::Undef(name) => name.to_string(),
        Action::Include(include) => include.path.display().to_string(),
    }
}

/// **What a fold does when it reaches an `#include`** — the two shapes of recursion, behind one trait.
///
/// The answer is the state *after* the include, or `None` when the file cannot be read (the include is dropped, as a
/// compiler would drop a missing header it was told to skip). Implemented by [`Nested`] — compose it and carry on —
/// and by `cache::env::unit`'s recorder — compose it, remember every file's entry state, carry on.
pub trait IncludeHandler {
    fn include(&mut self, path: &Path, env: &Env, depth: usize) -> Option<Env>;
}

/// **Compose the environment a file leaves behind**, starting from `incoming`.
///
/// `look_up` answers a path with the skeleton and the guards of the file it names — the two things a fold needs from
/// a summary — and is the caller's job because the summaries live in an index that outlives this call.
pub fn compose(
    skeleton: &Skeleton,
    guards: &SummaryGuards,
    incoming: &Env,
    seed: &dyn MacroValues,
    look_up: &mut impl FnMut(&Path) -> Option<(Skeleton, SummaryGuards)>,
) -> Composed {
    fold(
        skeleton,
        guards,
        incoming,
        seed,
        &mut Nested { seed, look_up },
        0,
    )
}

/// The plain recursion: an include is another fold, and its final state is what the caller continues from.
struct Nested<'a> {
    seed: &'a dyn MacroValues,
    look_up: &'a mut dyn FnMut(&Path) -> Option<(Skeleton, SummaryGuards)>,
}

impl IncludeHandler for Nested<'_> {
    fn include(&mut self, path: &Path, env: &Env, depth: usize) -> Option<Env> {
        let (skeleton, guards) = (self.look_up)(path)?;
        Some(fold(&skeleton, &guards, env, self.seed, self, depth + 1).final_env)
    }
}

/// **The fold itself** — the one loop both shapes of recursion share.
pub(crate) fn fold(
    skeleton: &Skeleton,
    guards: &SummaryGuards,
    incoming: &Env,
    seed: &dyn MacroValues,
    handler: &mut dyn IncludeHandler,
    depth: usize,
) -> Composed {
    Fold {
        env: incoming.clone(),
        history: vec![(0, incoming.clone())],
        after_each_include: Vec::new(),
        unknown_guards: 0,
        unknown: Vec::new(),
        skipped_includes: Vec::new(),
        depth_exceeded: 0,
        seed,
        handler,
        depth,
    }
    .run(skeleton, guards)
}

struct Fold<'a> {
    env: Env,
    /// `(offset, state)` after each applied directive, plus the incoming state at offset 0 — the table
    /// [`SummaryGuards::visibility_of`] asks for, and the reason a condition is decided where it was written.
    history: Vec<(usize, Env)>,
    after_each_include: Vec<(usize, Env)>,
    unknown_guards: usize,
    unknown: Vec<(usize, String)>,
    skipped_includes: Vec<(usize, String, &'static str, String)>,
    depth_exceeded: usize,
    seed: &'a dyn MacroValues,
    handler: &'a mut dyn IncludeHandler,
    depth: usize,
}

impl Fold<'_> {
    fn run(mut self, skeleton: &Skeleton, guards: &SummaryGuards) -> Composed {
        // **A file whose own include guard is not taken contributes nothing** — not even its `#define`s, which the
        // summary stores as unconditional (see `SummaryGuards::own_guard_holds`). This is what makes the second
        // `#include` of a guarded header leave the environment exactly as the first did.
        let held = guards.own_guard_holds(&self.env);
        if held != Some(true) {
            if held.is_none() {
                self.unknown_guards += 1;
            }
            return self.finish(skeleton);
        }
        for step in skeleton.steps() {
            let visibility = guards.visibility_of(step.guard, step.at, |condition_at| {
                Over::new(self.env_at(condition_at), self.seed)
            });

            match visibility {
                Visibility::Active => {}
                Visibility::Inactive => {
                    self.note_a_skipped_include(step, guards, "Inactive");
                    continue;
                }
                Visibility::Unknown => {
                    self.note_a_skipped_include(step, guards, "Unknown");
                    self.unknown_guards += 1;
                    if self.unknown.len() < UNKNOWN_EXAMPLES {
                        self.unknown.push((
                            step.at,
                            format!(
                                "{} under {}",
                                step_label(step),
                                guard_text(guards, step.guard)
                            ),
                        ));
                    }
                    continue;
                }
            }
            self.apply(step);
            self.history.push((step.at, self.env.clone()));
        }
        self.finish(skeleton)
    }

    fn finish(self, skeleton: &Skeleton) -> Composed {
        Composed {
            final_env: self.env,
            after_each_include: self.after_each_include,
            unknown_guards: self.unknown_guards,
            unknown: self.unknown,
            skipped_includes: self.skipped_includes,
            unresolved_includes: skeleton.unresolved_includes(),
            depth_exceeded: self.depth_exceeded,
        }
    }

    /// **Remember an include this fold did not take**, so a probe can say which one and why. Only includes are
    /// recorded: a skipped #define is one name, and the multiplier that matters is the subtree behind an
    /// #include.
    fn note_a_skipped_include(
        &mut self,
        step: &Step,
        guards: &SummaryGuards,
        visibility: &'static str,
    ) {
        if !matches!(step.action, Action::Include(_))
            || self.skipped_includes.len() >= UNKNOWN_EXAMPLES
        {
            return;
        }
        self.skipped_includes.push((
            step.at,
            step_label(step),
            visibility,
            guard_text(guards, step.guard),
        ));
    }

    fn apply(&mut self, step: &Step) {
        match &step.action {
            Action::Define(definition) => {
                self.env = self.env.define(definition.clone());
            }
            Action::Undef(name) => {
                self.env = self.env.undef(name);
            }
            Action::Include(include) => {
                if self.depth >= MAX_DEPTH {
                    self.depth_exceeded += 1;
                    return;
                }
                if let Some(after) = (self.handler).include(&include.path, &self.env, self.depth) {
                    self.env = after.clone();
                    self.after_each_include.push((step.at, after));
                }
            }
        }
    }

    /// The state at `offset`: the last directive applied at or before it, or the incoming state.
    ///
    /// A linear scan from the end is the right shape here: a condition is asked about an offset the fold has just
    /// passed (the condition of a region whose directive it is deciding), so the answer is at or near the end. A
    /// condition asked about an old offset — a region opened before several includes, with a fact after them — is the
    /// case this stays correct for at the cost of a walk back, and it is not the common one.
    fn env_at(&self, offset: usize) -> Env {
        for (at, env) in self.history.iter().rev() {
            if *at <= offset {
                return env.clone();
            }
        }
        self.history
            .first()
            .map(|(_, env)| env.clone())
            .unwrap_or_default()
    }
}
