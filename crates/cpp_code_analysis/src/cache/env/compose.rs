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

use std::path::Path;

use crate::Visibility;
use crate::cache::summary::SummaryGuards;

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
}

/// **Compose the environment a file leaves behind**, starting from `incoming`.
///
/// `look_up` answers a path with the skeleton and the guards of the file it names — the two things a fold needs from
/// a summary — and is the caller's job because the summaries live in an index that outlives this call.
pub fn compose(
    skeleton: &Skeleton,
    guards: &SummaryGuards,
    incoming: &Env,
    look_up: &mut impl FnMut(&Path) -> Option<(Skeleton, SummaryGuards)>,
) -> Composed {
    Fold {
        env: incoming.clone(),
        history: vec![(0, incoming.clone())],
        after_each_include: Vec::new(),
        unknown_guards: 0,
        depth_exceeded: 0,
        look_up,
    }
    .run(skeleton, guards, 0)
}

struct Fold<'a> {
    env: Env,
    /// `(offset, state)` after each applied directive, plus the incoming state at offset 0 — the table
    /// [`SummaryGuards::visibility_of`] asks for, and the reason a condition is decided where it was written.
    history: Vec<(usize, Env)>,
    after_each_include: Vec<(usize, Env)>,
    unknown_guards: usize,
    depth_exceeded: usize,
    look_up: &'a mut dyn FnMut(&Path) -> Option<(Skeleton, SummaryGuards)>,
}

impl<'a> Fold<'a> {
    fn run(mut self, skeleton: &Skeleton, guards: &SummaryGuards, depth: usize) -> Composed {
        // **A file whose own include guard is not taken contributes nothing** — not even its `#define`s, which the
        // summary stores as unconditional (see `SummaryGuards::own_guard_holds`). This is what makes the second
        // `#include` of a guarded header leave the environment exactly as the first did.
        match guards.own_guard_holds(&self.env) {
            Some(true) => {}
            held => {
                if held.is_none() {
                    self.unknown_guards += 1;
                }
                return Composed {
                    final_env: self.env,
                    after_each_include: self.after_each_include,
                    unknown_guards: self.unknown_guards,
                    unresolved_includes: skeleton.unresolved_includes(),
                    depth_exceeded: self.depth_exceeded,
                };
            }
        }
        for step in skeleton.steps() {
            let visibility = guards.visibility_of(step.guard, step.at, |condition_at| {
                self.env_at(condition_at)
            });
            match visibility {
                Visibility::Active => {}
                Visibility::Inactive => continue,
                Visibility::Unknown => {
                    self.unknown_guards += 1;
                    continue;
                }
            }
            self.apply(step, depth);
            self.history.push((step.at, self.env.clone()));
        }
        Composed {
            final_env: self.env,
            after_each_include: self.after_each_include,
            unknown_guards: self.unknown_guards,
            unresolved_includes: skeleton.unresolved_includes(),
            depth_exceeded: self.depth_exceeded,
        }
    }

    fn apply(&mut self, step: &Step, depth: usize) {
        match &step.action {
            Action::Define(definition) => {
                self.env = self.env.define(definition.clone());
            }
            Action::Undef(name) => {
                self.env = self.env.undef(name);
            }
            Action::Include(include) => {
                if depth >= MAX_DEPTH {
                    self.depth_exceeded += 1;
                    return;
                }
                let Some((child, child_guards)) = (self.look_up)(&include.path) else {
                    return;
                };
                let composed = Fold {
                    env: self.env.clone(),
                    history: vec![(0, self.env.clone())],
                    after_each_include: Vec::new(),
                    unknown_guards: 0,
                    depth_exceeded: 0,
                    look_up: self.look_up,
                }
                .run(&child, &child_guards, depth + 1);
                self.unknown_guards += composed.unknown_guards;
                self.depth_exceeded += composed.depth_exceeded;
                self.env = composed.final_env;
                self.after_each_include.push((step.at, self.env.clone()));
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
