//! Guards: which conditional regions a piece of code sits inside, and whether it is being compiled.
//!
//! # Why a guard is a list, not a boolean
//!
//! The decision this layer supports is *not* "take this branch and drop the other". A compiler does
//! that; an editor cannot, because both branches are code somebody is writing and both have to stay in
//! the tree, in the index, and in the file's outline. What the editor needs to know is narrower:
//!
//! * Is this declaration visible in the configuration we are analysing under?
//! * If not, is it because the configuration says so, or because we cannot tell?
//!
//! So a guard is the list of conditions a region sits inside, each carrying its own truth value, and
//! the answer is computed from the list rather than from a single flag. Code at file scope has an empty
//! guard, which is `Active` — the common case, and it stays cheap.
//!
//! # Why the list is nested and not flat
//!
//! An `#elif` or `#else` is an alternative to the branches *at its own level*, and only to those. In
//!
//! ```text
//! #if A
//! #else          <- taken when A is false
//! #  if B
//! #  else        <- taken when B is false
//! #  endif
//! #endif
//! ```
//!
//! a flat list of `[A, !A, B, !B]` cannot say which `#else` pairs with which `#if`, and the second
//! `#else` would be compared against `A` as well. So a guard is a list of *regions*, and each region
//! holds the chain of branches written for it.
//!
//! # `Unknown` is the point
//!
//! ```text
//! #if defined(_WIN32)
//! void win_only();
//! #else
//! void posix_only();
//! #endif
//! ```
//!
//! On a machine where the configuration does not mention `_WIN32`, the honest answers are "the first is
//! not compiled" and "the second is" — *if* the configuration is complete. If the analysis has no
//! configuration at all, both are `Unknown`, and a consumer that renders `Unknown` as "still visible,
//! but do not diagnose inside it" gets the editor behaviour people expect: completion offers both, and
//! neither is called an error.

use cpp_parser::SourceRange;

use crate::{
    condition::{MacroValues, evaluate},
    directive::DirectiveKind,
    token::Token,
};

/// One branch of a conditional region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// Which directive wrote it: `#if`, `#ifdef`, `#ifndef`, `#elif`, `#else`.
    pub kind: DirectiveKind,
    /// The expression's tokens, for the `#if` and `#elif` forms.
    pub tokens: Vec<Token>,
    /// The name tested, for `#ifdef` and `#ifndef`.
    pub name: Option<Box<str>>,
    /// Where the branch was written.
    pub range: SourceRange,
}

impl Branch {
    /// Does this branch's own expression hold?
    ///
    /// `None` when it cannot be decided. An `#else` has no expression, so it always "holds" — whether
    /// the region as a whole takes it is a question about the branches before it.
    ///
    /// Evaluating on demand rather than caching a value is deliberate: whether a condition holds depends
    /// on the macro table *at that point in the file*, and a guard cloned onto a declaration carries no
    /// position of its own. The caller has the position; this does not need it.
    pub fn holds(&self, macros: &impl MacroValues) -> Option<bool> {
        match self.kind {
            DirectiveKind::If | DirectiveKind::Elif => evaluate(&self.tokens, macros).is_true(),
            DirectiveKind::Ifdef => Some(
                self.name
                    .as_deref()
                    .is_some_and(|name| macros.lookup(name).is_some()),
            ),
            DirectiveKind::Ifndef => Some(
                !self
                    .name
                    .as_deref()
                    .is_some_and(|name| macros.lookup(name).is_some()),
            ),
            DirectiveKind::Else => Some(true),
            _ => None,
        }
    }
}

/// One conditional region: the chain of branches written for a single `#if`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The branches in source order; the first is always the opener.
    pub branches: Vec<Branch>,
    /// Which branch is in force at the position being asked about.
    pub active_branch: usize,
}

impl Region {
    /// Is the branch in force at this position the one that gets compiled?
    ///
    /// The rule is the standard's, in order: the first branch whose own expression holds is taken; a
    /// branch whose expression does not hold is not; and if any earlier branch is undecidable, then
    /// whether *this* branch is taken depends on it.
    pub fn visibility(&self, macros: &impl MacroValues) -> Option<bool> {
        // Whether some branch before the active one is undecidable. If it is, then even a definite
        // "yes" from the active branch does not settle the region: the undecidable one might have been
        // taken first. `#if X` / `#elif 1` is the case — `1` holds, and yet whether the region is taken
        // depends entirely on `X`.
        let mut unknown_before = false;

        for (index, branch) in self.branches.iter().enumerate() {
            let holds = branch.holds(macros);

            if index < self.active_branch {
                match holds {
                    // An earlier branch is taken, so this one is not.
                    Some(true) => return Some(false),
                    Some(false) => {}
                    None => unknown_before = true,
                }
                continue;
            }

            if index > self.active_branch {
                break;
            }

            return match (holds, unknown_before) {
                (None, _) => None,
                // The active branch does not hold, and an earlier branch might have been taken
                // instead. Either way this branch is not the one compiled, so the answer is a
                // definite "no": its own expression already rules it out.
                (Some(false), _) => Some(false),
                // The active branch holds, but an earlier one might have been taken instead — so
                // whether *this* region is taken depends on that earlier one.
                (Some(true), true) => None,
                (Some(true), false) => Some(true),
            };
        }

        // The active index is past the end, which should not happen. "Cannot tell" is the safe answer
        // rather than a panic in an editor.
        None
    }
}

/// Whether the code inside a guard is being compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Every condition holds, so this code is compiled. Code at file scope is always this.
    Active,
    /// Some condition does not hold, so this code is not compiled under the configuration in hand. It
    /// stays in the tree and in the index; a consumer greys it out rather than dropping it.
    Inactive,
    /// At least one condition cannot be decided, and none of the ones that *can* be decided rules the
    /// region out.
    Unknown,
}

impl Visibility {
    /// Should a consumer treat this code as reachable?
    ///
    /// `Active` and `Unknown` both answer yes: code whose visibility is unknown is code that might be
    /// compiled, and hiding it would be the wrong guess. This is the predicate completion should use.
    pub fn is_reachable(self) -> bool {
        !matches!(self, Visibility::Inactive)
    }

    /// Should a consumer report diagnostics inside this code?
    ///
    /// Only `Active`. Reporting an error in a branch that is not compiled is a false positive, and an
    /// editor-facing tool that reports false positives gets switched off.
    pub fn is_diagnosable(self) -> bool {
        matches!(self, Visibility::Active)
    }
}

/// The conditional regions an item sits inside, outermost first.
///
/// Cloned freely rather than shared: a guard is a handful of small values, and a file has one per
/// `#if`, so the copying is bounded by the number of conditionals rather than by the number of
/// declarations that consult it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Guard {
    regions: Vec<Region>,
}

impl Guard {
    /// The guard of code at file scope: nothing to satisfy.
    pub fn empty() -> Self {
        Guard {
            regions: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// How deeply nested the conditionals are.
    pub fn depth(&self) -> usize {
        self.regions.len()
    }

    /// The branch in force at the innermost level, if the code sits inside any conditional at all.
    ///
    /// This is what a UI asks to draw the `#if` a line belongs to.
    pub fn innermost_branch(&self) -> Option<&Branch> {
        let region = self.regions.last()?;
        region.branches.get(region.active_branch)
    }

    /// Report whether this code is compiled under `macros`.
    ///
    /// A region is `Inactive` as soon as one region is known not to be taken, and `Unknown` when none
    /// is ruled out but one cannot be decided. That ordering matters: code inside `#if 0` is inactive
    /// even if an inner `#if` is undecidable, because the outer one already settled it.
    pub fn visibility(&self, macros: &impl MacroValues) -> Visibility {
        let mut unknown = false;

        for region in &self.regions {
            match region.visibility(macros) {
                Some(true) => {}
                Some(false) => return Visibility::Inactive,
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

/// Builds guards by walking a file's directives in order.
///
/// Exists as a type rather than as a function over a slice because the interesting state is the stack,
/// and because a caller may want to interleave its own walk — asking "what guard is in force here?" as
/// it moves through the file.
#[derive(Debug, Clone, Default)]
pub struct GuardStack {
    stack: Vec<Region>,
}

impl GuardStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// The guard in force, as a value a consumer can attach to a declaration.
    pub fn guard(&self) -> Guard {
        Guard {
            regions: self.stack.clone(),
        }
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Account for a conditional directive.
    ///
    /// Directives that are not conditional are ignored, so a caller can feed a whole file through
    /// without deciding what matters.
    pub fn observe(&mut self, kind: DirectiveKind, branch: Branch) {
        match kind {
            DirectiveKind::If | DirectiveKind::Ifdef | DirectiveKind::Ifndef => {
                self.stack.push(Region {
                    branches: vec![branch],
                    active_branch: 0,
                });
            }
            DirectiveKind::Elif | DirectiveKind::Else => {
                if let Some(region) = self.stack.last_mut() {
                    // A new branch becomes the one in force for everything after it. Whether it is
                    // *taken* is a question for `visibility`; "which branch are we lexically inside"
                    // is this, and the two are different questions.
                    region.branches.push(branch);
                    region.active_branch = region.branches.len() - 1;
                }
            }
            DirectiveKind::Endif => {
                self.stack.pop();
            }
            _ => {}
        }
    }
}
