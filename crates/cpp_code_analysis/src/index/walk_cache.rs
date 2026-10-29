//! **A file's contribution to a translation unit, shared between units** — `docs/walk-redesign.md` §4.2 and §8 step 2.
//!
//! # What a contribution is
//!
//! When the walk enters a file it produces, in order: the macro state changes the file makes, the timeline frames and
//! events for the file and everything it includes, the unconditional facts that become the unit's evidence, the bodies
//! of conditional definitions that came out in force, and the `#define`s it decided are not compiled. Those are a pure
//! function of *(the file, the values of the names its conditions read)* — which is the projection whose reuse ratio
//! was measured at **37×** (`crate::index::walk_census`, `docs/walk-redesign.md` §11).
//!
//! So the contribution is stored keyed by that projection, and a later unit that reaches the same file with the same
//! projection **splices** it instead of walking: the frames and events are copied with their indices rebased, and the
//! state changes are replayed into the unit's own table. Nothing is re-decided and no text is re-read.
//!
//! # Why the entries are stored as positions and not as facts
//!
//! `Walked::entries` holds `&'a MacroFact` — borrowed from the summaries, which is exactly why a contribution cannot
//! hold them. The positions are enough: an entry is `(file, offset)` and the summary's own `macros` are in offset
//! order, so a splice finds each fact again with a binary search. That is a lookup per entry, against a walk per
//! entry, which is the trade this whole design is.
//!
//! # Invalidation, and why the key carries a revision
//!
//! A contribution depends on the **contents of every file in its subtree**, and a shorter key would go stale the
//! moment a header changed — silently, which is the one failure mode this crate refuses. So the key carries
//! [`crate::index::store::SummaryStore::revision`], a counter that only moves when a summary is dropped or invalidated:
//! one edit anywhere retires every contribution, which is conservative in the only safe direction. The counter is not
//! bumped by ordinary commits, or the cache would never survive two files in a row.
//!
//! **What one file contributed to a walk, kept across walks.**
//!
//! The table is process-wide (see [`Contributions`]: it used to be a `thread_local`, which multiplied it by the
//! size of the index pool) and bounded, because it is a memo: a miss costs one re-walk of one file.
use std::collections::HashMap;
use std::sync::Arc;

use crate::macros::MacroDef;
use crate::summary::{TuEvent, TuFrame};

/// **One state change a file makes**, in the order it makes it.
///
/// Order is the whole point: `#define X 1` followed by `#undef X` is not the same unit as the reverse, and a
/// contribution that stored only the final table would replay the wrong one.
#[derive(Debug, Clone)]
pub(crate) enum StateChange {
    /// A name put in force, with the definition and the position it was written at.
    Defined {
        name: Box<str>,
        definition: Arc<MacroDef>,
        file: std::path::PathBuf,
        at: usize,
    },
    /// A name taken out of force. The file is carried for the same reason as above: a contribution keeps the changes
    /// **it** made, and a walk interleaves a file's own facts with the subtrees it includes.
    Undefined {
        name: Box<str>,
        file: std::path::PathBuf,
        /// Where the `#undef` was written — the key a splice merges the changes with the events and the edges by.
        at: usize,
    },
}

/// **One entry of a unit's evidence**, as a position rather than a borrow — see the module documentation.
#[derive(Debug, Clone)]
pub(crate) struct EntryPosition {
    /// Where the name becomes visible: the file's own depth in the walk, **relative** to this contribution's root.
    pub(crate) from_offset: isize,
    pub(crate) file: std::path::PathBuf,
    pub(crate) at: usize,
}

/// **A body a condition guarded that came out in force** — owned, because a contribution outlives the walk.
#[derive(Debug, Clone)]
pub(crate) struct ConditionalBody {
    pub(crate) name: Box<str>,
    pub(crate) function_like: bool,
    pub(crate) parameters: Option<Box<str>>,
    pub(crate) body: Box<str>,
}

/// **What one file contributed** — its own frame, its own facts, and the edges it took.
///
/// **Not its subtree.** The first version stored the subtree, and a chain of depth *d* then kept *d* copies of every
/// file's work; measured, that cost more than the walks it saved (`docs/walk-redesign.md` §11.3). A contribution is
/// one file, and the edges are how a splice reaches the rest — the memoized traversal §4.3 describes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Contribution {
    /// The file this contribution is for.
    pub(crate) file: std::path::PathBuf,
    /// The file writes `#pragma once`, so it is entered at most once per unit.
    pub(crate) visit_once: bool,
    /// The events **this file wrote**, every one of them with `frame == 0` once spliced.
    pub(crate) events: Vec<TuEvent>,
    /// The state changes this file made, in order.
    pub(crate) changes: Vec<StateChange>,
    /// **The include edges this file took**, in order: where in the file, and the key of what was walked there.
    pub(crate) edges: Vec<(usize, u64)>,
    /// The conditional bodies that came out in force.
    pub(crate) bodies: Vec<ConditionalBody>,
    /// The `#define`s *this file* decided are not compiled.
    pub(crate) dead: Vec<usize>,
    pub(crate) conditional_facts: usize,
    pub(crate) facts_in_force: usize,
}

impl StateChange {
    /// The file that made the change — a contribution keeps only its own, and a walk interleaves a file with its
    /// includes.
    pub(crate) fn file(&self) -> &std::path::Path {
        match self {
            StateChange::Defined { file, .. } => file,
            StateChange::Undefined { file, .. } => file,
        }
    }

    /// **Where in its file the change was written** — the key a splice interleaves by.
    ///
    /// A file's own facts and the includes it takes are one sequence in the source, and the walk visits them in offset
    /// order. A contribution that replayed all of its changes before recursing its edges would apply a `#define`
    /// written after an `#include` before it — a different program, and measured as one failing semantics test
    /// (`docs/walk-redesign.md` §11.5).
    pub(crate) fn at(&self) -> usize {
        match self {
            StateChange::Defined { at, .. } => *at,
            StateChange::Undefined { at, .. } => *at,
        }
    }
}

/// A frame as a contribution stores it: the parent is relative, so a subtree can be moved.
#[derive(Debug, Clone)]
pub(crate) struct CachedFrame {
    pub(crate) file: std::path::PathBuf,
    /// `None` for the contribution's root, whose parent is the frame the splice hangs it under.
    pub(crate) parent: Option<u32>,
    pub(crate) from_in_parent: usize,
    pub(crate) visit_once: bool,
    /// The first event of this frame's subtree, **relative** to the contribution's first event.
    pub(crate) entry_seq: u32,
    /// One past this frame's subtree, as a frame index **relative** to the contribution's first frame.
    pub(crate) tout: u32,
}

/// **How many contributions a session keeps** — 512 files worth of walk work, against a project of thousands of
/// files. It used to be unbounded, and every file the index walked left its events, state changes and guarded bodies
/// in the table for the life of the process.
const MAX_CONTRIBUTIONS: usize = 512;

/// **The identities a contribution is filed under.**
///
/// Four things, and each is a way the answer could differ: the file, the contents of the file, the values of the
/// names its conditions read (the projection), and the store's revision — which stands for the contents of every file
/// the subtree reaches, because it moves whenever any summary is dropped.
pub(crate) fn key(file: &std::path::Path, content: u64, projection: u64) -> u64 {
    let mut hash = crate::index::walk_census::hash(file.to_string_lossy().as_bytes());
    for part in [content, projection] {
        hash = hash.rotate_left(7) ^ part;
    }
    hash
}

/// **The contributions one session has recorded**, keyed by [`key`], oldest first in `order`.
///
/// # Why this is a session's table and not a process-wide one
///
/// It was a `thread_local` (one copy of every file's timeline per thread), then a `static Mutex` (one copy for the
/// whole process, outliving the session it was built for). Both say the same wrong thing: a memo whose owner is
/// nobody. `docs/memory-audit.md` §3.2 states the rule this now follows — **state belongs to an owner** — so the
/// table is a field of [`crate::index::store::SummaryStore`], the store whose `revision` keys it, and a second
/// session over another project has its own.
///
/// # Why its size is bounded
///
/// It is a **memo**: a file whose contribution is gone is walked again, exactly as on a cold start, and no answer
/// depends on it. [`MAX_CONTRIBUTIONS`] is what keeps "the analysis remembers every file it ever walked" from
/// becoming "the analysis keeps a second copy of the project for ever".
#[derive(Default)]
pub struct WalkCache {
    table: std::sync::Mutex<Table>,
}

#[derive(Default)]
struct Table {
    held: HashMap<u64, Arc<Contribution>>,
    order: std::collections::VecDeque<u64>,
}

impl Table {
    /// Drop the oldest until the table is within its bound.
    fn evict(&mut self) {
        while self.held.len() > MAX_CONTRIBUTIONS {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.held.remove(&oldest);
        }
    }
}

impl WalkCache {
    pub fn new() -> WalkCache {
        WalkCache::default()
    }

    /// A poisoned lock is not a reason to lose an answer: the table is a memo, and a thread that panicked while
    /// holding it did not leave a fact half-written — every mutation is one `insert` or one `remove`.
    fn locked(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// **The contribution filed under this key**, if this session has one.
    pub(crate) fn lookup(&self, key: u64) -> Option<Arc<Contribution>> {
        self.locked().held.get(&key).cloned()
    }

    /// **File a contribution**, replacing whatever was there.
    pub(crate) fn store(&self, key: u64, contribution: Contribution) {
        let mut table = self.locked();
        table.held.insert(key, Arc::new(contribution));
        table.order.push_back(key);
        table.evict();
    }

    /// **What the table is holding** — `(entries, estimated bytes)` for `MemoryReport`.
    pub fn accounting(&self) -> (usize, usize) {
        let table = self.locked();
        let mut bytes = 0usize;
        for contribution in table.held.values() {
            bytes += std::mem::size_of::<Contribution>() + contribution.file.as_os_str().len();
            for event in &contribution.events {
                bytes += std::mem::size_of::<crate::summary::TuEvent>()
                    + event.name.len()
                    + event.body_text.as_ref().map_or(0, |text| text.len())
                    + event.parameters.as_ref().map_or(0, |text| text.len());
            }
            bytes += contribution.changes.len() * std::mem::size_of::<StateChange>();
            bytes += contribution.edges.len() * std::mem::size_of::<(usize, u64)>();
            bytes += contribution.bodies.len() * std::mem::size_of::<crate::ConditionalBody>();
            bytes += contribution.dead.len() * std::mem::size_of::<usize>();
        }
        (table.held.len(), bytes)
    }

    /// **Forget everything.** Called when the store's revision moves, so that a stale contribution cannot outlive the
    /// summaries it was made from even if its key were somehow reused.
    pub(crate) fn clear(&self) {
        let mut table = self.locked();
        table.held.clear();
        table.order.clear();
        drop(table);
        // The projection memo is invalidated by the same edits, and keeping them together is the only way they
        // cannot disagree about which state of a file they describe.
        crate::index::walk_census::forget_projections();
    }

    /// **Everything this session holds**, for the codec to write.
    pub(crate) fn saved(&self) -> Vec<(u64, Contribution)> {
        self.locked()
            .held
            .iter()
            .map(|(key, contribution)| (*key, (**contribution).clone()))
            .collect()
    }

    /// **Put a decoded cache back** — a warm start, and nothing else.
    pub(crate) fn restore(&self, entries: Vec<(u64, Contribution)>) {
        let mut table = self.locked();
        table.held.clear();
        table.order.clear();
        for (key, contribution) in entries {
            table.held.insert(key, Arc::new(contribution));
            table.order.push_back(key);
        }
        // **The bound applies to a restored table too**: a cache file written before the bound existed can name more
        // files than the table should hold, and restoring all of them would put back what the bound removed.
        table.evict();
    }

    /// **How many contributions are held** — the number a probe reports, so that "the cache is on" is not something
    /// to infer from a wall clock.
    pub(crate) fn held(&self) -> usize {
        self.locked().held.len()
    }
}
/// **The frame and events a splice puts in the timeline**, rebased onto the site's indices.
///
/// A contribution is one file, so this is one frame: the site supplies the parent and the offset inside it, and the
/// caller closes it with `leave` once the edges have been spliced. An event's `frame` becomes the frame's own id.
pub(crate) fn rebased(
    contribution: &Contribution,
    frame: u32,
    event_base: u32,
    parent: Option<u32>,
    from_in_parent: usize,
) -> (TuFrame, Vec<TuEvent>) {
    let built = TuFrame {
        file: contribution.file.clone(),
        parent,
        from_in_parent,
        visit_once: contribution.visit_once,
        entry_seq: event_base,
        // Closed by the caller's `leave`, exactly as a walked frame is.
        tout: u32::MAX,
    };

    let mut events = contribution.events.clone();
    for event in &mut events {
        event.frame = frame;
    }

    (built, events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_event() -> TuEvent {
        TuEvent {
            name: std::sync::Arc::from("X"),
            function_like: None,
            body: None,
            body_text: None,
            parameters: None,
            body_range: None,
            frame: 0,
            at: 7,
            unconditional: true,
        }
    }

    #[test]
    fn a_spliced_frame_takes_its_parent_from_the_site_and_its_events_take_the_frame() {
        let contribution = Contribution {
            file: std::path::PathBuf::from("/p/a.h"),
            visit_once: true,
            events: vec![an_event()],
            ..Contribution::default()
        };
        let (frame, events) = rebased(&contribution, 5, 41, Some(2), 33);
        assert_eq!(frame.parent, Some(2));
        assert_eq!(frame.from_in_parent, 33);
        assert_eq!(frame.entry_seq, 41);
        assert_eq!(frame.tout, u32::MAX);
        assert_eq!(events[0].frame, 5);
        assert_eq!(events[0].at, 7);
    }

    #[test]
    fn the_key_moves_with_the_file_its_contents_and_the_projection() {
        let base = key(std::path::Path::new("/p/a.cpp"), 1, 3);
        assert_ne!(
            base,
            key(std::path::Path::new("/p/a.cpp"), 9, 3),
            "contents"
        );
        assert_ne!(
            base,
            key(std::path::Path::new("/p/a.cpp"), 1, 9),
            "projection"
        );
        assert_ne!(base, key(std::path::Path::new("/p/b.cpp"), 1, 3), "file");
        assert_eq!(base, key(std::path::Path::new("/p/a.cpp"), 1, 3));
    }
}
