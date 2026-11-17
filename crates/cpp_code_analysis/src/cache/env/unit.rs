//! **Every file's environment, from one fold of the unit** — the shape a differential test compares against the walk,
//! and the shape S1 needs: the state at each inclusion site, not just at the end of the root.
//!
//! The walk answers this with a timeline: one event per directive in the whole closure, and a file's view is a
//! position in it. The fold answers it with a traversal: visit the root's steps in order, and when one is an
//! `#include`, remember the state it starts from and visit that file with it. The two agree by construction only if
//! both mean the same thing by "the state at a file's entry", and that is the thing Gate 0 measures.
//!
//! # What "entry" means here
//!
//! The state **before the file's own first directive**, which is the walk's *inherited prefix*: everything emitted
//! before this frame was entered, including earlier siblings' facts. The walk's `environment_of` answers a different
//! question — "what this file sees from elsewhere", which additionally includes the file's own includes' facts at the
//! offsets they become live — so a comparison that wants one number per file compares *this* against the walk's view
//! only for the names the file's own subtree does not define. `cache::env::differential` is the caller, and it says
//! the same in its own words.
//!
//! # First entry only
//!
//! A file included twice has two entry states and the walk records the **first** (`TranslationUnit::entered`). This
//! records the first too, for the same reason: a second entry is a second question, and what a unit-wide table is for
//! is "where does this file come from" rather than "every path that reached it".

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::preprocess::condition::MacroValues;

use super::compose::{IncludeHandler, fold};
use super::{Env, Skeleton};
use crate::cache::summary::SummaryGuards;

/// One file of a unit, with the states a fold gave it.
#[derive(Debug, Clone)]
pub struct ComposedFile {
    pub path: PathBuf,
    /// The state before its own first directive — the walk's inherited prefix.
    pub at_entry: Env,
    /// The state after its own directives and everything it includes.
    pub at_exit: Env,
    /// How deep in `#include`s it was; 0 for the root.
    pub depth: usize,
    /// Directives in this file whose region the fold could not decide.
    pub unknown_guards: usize,
}

/// What composing a unit produced.
#[derive(Debug, Clone, Default)]
pub struct UnitComposition {
    pub files: Vec<ComposedFile>,
    /// Directives whose region could not be decided, over every file in the unit.
    pub unknown_guards: usize,
    /// Includes that would have nested past [`MAX_DEPTH`].
    pub depth_exceeded: usize,
    /// Includes whose target did not resolve, over every file in the unit.
    pub unresolved_includes: usize,
    /// **The undecided directives**, as (file, offset, what), capped — the attribution a probe prints.
    pub unknown: Vec<(PathBuf, usize, String)>,
}

impl UnitComposition {
    /// The file's record, when the unit reached it.
    pub fn file(&self, path: &Path) -> Option<&ComposedFile> {
        self.files.iter().find(|file| file.path == path)
    }
}

/// **Compose a whole unit**: the state at every file's entry and exit, from one traversal of the summaries.
pub fn compose_unit(
    root: &Path,
    incoming: &Env,
    seed: &dyn MacroValues,
    look_up: &mut impl FnMut(&Path) -> Option<(Skeleton, SummaryGuards)>,
) -> UnitComposition {
    let mut recorder = Recording {
        seed,
        look_up,
        files: Vec::new(),
        seen: HashSet::new(),
        unknown_guards: 0,
        depth_exceeded: 0,
        unresolved_includes: 0,
        unknown: Vec::new(),
    };
    let Some((skeleton, guards)) = (recorder.look_up)(root) else {
        return UnitComposition::default();
    };
    recorder.seen.insert(root.to_path_buf());
    // The root's record is written before its fold and completed after, so the file list is in entry order.
    recorder.files.push(ComposedFile {
        path: root.to_path_buf(),
        at_entry: incoming.clone(),
        at_exit: incoming.clone(),
        depth: 0,
        unknown_guards: 0,
    });
    let composed = fold(&skeleton, &guards, incoming, seed, &mut recorder, 0);
    recorder.unknown_guards += composed.unknown_guards;
    recorder.depth_exceeded += composed.depth_exceeded;
    recorder.unresolved_includes += composed.unresolved_includes;
    recorder.files[0].at_exit = composed.final_env;
    recorder.files[0].unknown_guards = composed.unknown_guards;
    for (offset, what) in composed.unknown {
        if recorder.unknown.len() < super::compose::UNKNOWN_EXAMPLES {
            recorder.unknown.push((root.to_path_buf(), offset, what));
        }
    }
    UnitComposition {
        files: recorder.files,
        unknown_guards: recorder.unknown_guards,
        depth_exceeded: recorder.depth_exceeded,
        unresolved_includes: recorder.unresolved_includes,
        unknown: recorder.unknown,
    }
}

struct Recording<'a> {
    seed: &'a dyn MacroValues,
    look_up: &'a mut dyn FnMut(&Path) -> Option<(Skeleton, SummaryGuards)>,
    files: Vec<ComposedFile>,
    seen: HashSet<PathBuf>,
    unknown_guards: usize,
    depth_exceeded: usize,
    unresolved_includes: usize,
    unknown: Vec<(PathBuf, usize, String)>,
}

impl IncludeHandler for Recording<'_> {
    fn include(&mut self, path: &Path, env: &Env, depth: usize) -> Option<Env> {
        let (skeleton, guards) = (self.look_up)(path)?;
        let first = self.seen.insert(path.to_path_buf());
        let composed = fold(&skeleton, &guards, env, self.seed, self, depth + 1);
        self.unknown_guards += composed.unknown_guards;
        self.depth_exceeded += composed.depth_exceeded;
        self.unresolved_includes += composed.unresolved_includes;
        if first {
            self.files.push(ComposedFile {
                path: path.to_path_buf(),
                at_entry: env.clone(),
                at_exit: composed.final_env.clone(),
                depth,
                unknown_guards: composed.unknown_guards,
            });
        }
        for (offset, what) in &composed.unknown {
            if self.unknown.len() < super::compose::UNKNOWN_EXAMPLES {
                self.unknown
                    .push((path.to_path_buf(), *offset, what.clone()));
            }
        }
        Some(composed.final_env)
    }
}
