use crate::{
    kind::{CppSyntaxKind, CppTokenKind},
    text::SourceRange,
};

#[derive(Debug, Clone)]
pub enum MarkEvent {
    NodeStart {
        kind: CppSyntaxKind,
        /// Forward parent: another `NodeStart` that must be opened *outside* this one. Uses
        /// [`NO_FORWARD_PARENT`] when there is none, so that position `0` stays addressable.
        parent: usize,
    },
    EatToken {
        kind: CppTokenKind,
        range: SourceRange,
    },
    NodeEnd,
    Trivia,
}

/// Sentinel for "this node has no forward parent". Must never collide with a real event index.
pub(crate) const NO_FORWARD_PARENT: usize = usize::MAX;

impl MarkEvent {
    pub fn none() -> Self {
        MarkEvent::NodeStart {
            kind: CppSyntaxKind::None,
            parent: NO_FORWARD_PARENT,
        }
    }
}

pub(crate) trait MarkerEventContainer {
    /// Number of nodes currently open. Snapshot it before a speculative or fallible sub-parse so
    /// that [`MarkerEventContainer::finish_marks_to`] can undo the damage on failure.
    fn get_mark_level(&self) -> usize;

    /// Register a newly opened node, so that it can be closed by `finish_marks_to`.
    fn push_mark(&mut self, position: usize);

    fn get_events(&mut self) -> &mut Vec<MarkEvent>;

    fn mark(&mut self, kind: CppSyntaxKind) -> Marker {
        let position = self.get_events().len();
        self.get_events().push(MarkEvent::NodeStart {
            kind,
            parent: NO_FORWARD_PARENT,
        });
        self.push_mark(position);
        Marker::new(position)
    }

    /// Close every node opened after `target` was snapshotted with
    /// [`MarkerEventContainer::get_mark_level`].
    ///
    /// This is the error-recovery counterpart to `?`: it turns "returning early from the middle
    /// of a grammar rule" from a silent tree corruption into a no-op, because the leaked
    /// `NodeStart` events stop being able to swallow the tokens that follow.
    ///
    /// Nodes are closed **by identity, not by counting**. Emitting `level - target` `NodeEnd`
    /// events is wrong: `target` is a snapshot taken by an *ancestor*, and the nodes opened since
    /// are not necessarily still nested the way a naive pop order assumes. Worse, closing by count
    /// can close a node whose rule is still on the stack, and then that rule's own `complete()`
    /// emits a *second* `NodeEnd` — which closes somebody else's node and unbalances everything
    /// after it.
    ///
    /// So the recovered nodes are detached **without** an event. Their `NodeEnd` is left to their
    /// owner: a rule still on the stack will reach its `complete()` and emit it then, paired with
    /// the matching `NodeStart`. Rules that are genuinely abandoned simply leave a `NodeStart`
    /// behind, which the tree builder balances by appending `NodeEnd`s at the end of the stream.
    fn finish_marks_to(&mut self, target: usize) {
        if self.get_mark_level() <= target {
            return;
        }

        for position in self.drain_marks(target) {
            self.close_mark(position, false);
        }
    }

    /// Remove and return the open-node positions above `target`.
    fn drain_marks(&mut self, target: usize) -> Vec<usize>;

    /// Mark a node as closed, optionally emitting its `NodeEnd`.
    ///
    /// The open set and the event stream are updated together here, so they can never disagree
    /// about whether a node has been closed. Returns whether an event was emitted.
    fn close_mark(&mut self, position: usize, want_event: bool) -> bool;

    /// Has a `NodeEnd` already been emitted for the node opened at `position`?
    fn mark_has_end_event(&self, position: usize) -> bool;

    /// Is the node opened at `position` still open?
    fn mark_is_open(&self, position: usize) -> bool;
}

pub(crate) struct Marker {
    pub position: usize,
}

impl Marker {
    pub fn new(position: usize) -> Self {
        Marker { position }
    }

    /// Overwrite the kind of the node this marker opened. Used by callers that discover the
    /// concrete construct only after parsing it (e.g. `class X;` vs `class X { ... };`).
    #[allow(dead_code)]
    pub fn set_kind<P: MarkerEventContainer>(&mut self, p: &mut P, kind: CppSyntaxKind) {
        match &mut p.get_events()[self.position] {
            MarkEvent::NodeStart { kind: k, .. } => *k = kind,
            _ => unreachable!(),
        }
    }

    pub fn complete<P: MarkerEventContainer>(self, p: &mut P) -> CompleteMarker {
        let kind = match p.get_events()[self.position] {
            MarkEvent::NodeStart { kind: k, .. } => k,
            _ => unreachable!(),
        };

        // Already closed with an event (normal double-complete, or recovery followed by the owner
        // catching up): nothing left to do.
        if p.mark_has_end_event(self.position) {
            return CompleteMarker {
                start: self.position,
                kind,
            };
        }

        // Detached by recovery without an event. That event is still owed — the tree builder pairs
        // it with the `NodeStart`. Emitting it here closes *this* node rather than the innermost
        // one, because by the time `complete` runs its owner has already closed everything that was
        // opened inside it.
        if !p.mark_is_open(self.position) {
            p.close_mark(self.position, true);
            return CompleteMarker {
                start: self.position,
                kind,
            };
        }

        // A node that never produced content carries no information, so drop it. Deregistering the
        // mark is still mandatory, or the open-node stack drifts upwards forever.
        if p.get_events().len() == self.position + 1 {
            p.close_mark(self.position, false);
            return CompleteMarker {
                start: EMPTY_NODE_POSITION,
                kind: CppSyntaxKind::None,
            };
        }

        p.close_mark(self.position, true);
        CompleteMarker {
            start: self.position,
            kind,
        }
    }

    /// Complete the node, but keep the result usable as a positioned node.
    ///
    /// `Marker::complete` degrades to `CompleteMarker::empty()` when the node turns out to be
    /// empty, which makes it impossible to tell "the construct was there but produced no
    /// events" from "the construct was not there". This variant keeps the original position so
    /// the caller can still call `set_kind` afterwards.
    #[allow(dead_code)]
    pub fn complete_non_empty<P: MarkerEventContainer>(self, p: &mut P) -> CompleteMarker {
        let kind = match p.get_events()[self.position] {
            MarkEvent::NodeStart { kind: k, .. } => k,
            _ => unreachable!(),
        };

        if p.mark_has_end_event(self.position) {
            return CompleteMarker {
                start: self.position,
                kind,
            };
        }

        if !p.mark_is_open(self.position) {
            p.close_mark(self.position, true);
            return CompleteMarker {
                start: self.position,
                kind,
            };
        }

        if p.get_events().len() == self.position + 1 {
            p.close_mark(self.position, false);
            return CompleteMarker {
                start: self.position,
                kind: CppSyntaxKind::None,
            };
        }

        p.close_mark(self.position, true);
        CompleteMarker {
            start: self.position,
            kind,
        }
    }

    #[allow(unused)]
    pub fn undo<P: MarkerEventContainer>(self, p: &mut P) {
        match &mut p.get_events()[self.position] {
            MarkEvent::NodeStart { kind, .. } => {
                *kind = CppSyntaxKind::None;
            }
            _ => unreachable!(),
        }
    }
}

pub(crate) struct CompleteMarker {
    start: usize,
    #[allow(dead_code)]
    pub kind: CppSyntaxKind,
}

/// Sentinel used by [`CompleteMarker::empty`]: it never points at a real event and every
/// consumer must treat it as "no node here".
pub(crate) const EMPTY_NODE_POSITION: usize = usize::MAX;

#[allow(dead_code)]
impl CompleteMarker {
    pub fn precede<P: MarkerEventContainer>(&self, p: &mut P, kind: CppSyntaxKind) -> Marker {
        let m = p.mark(kind);
        match &mut p.get_events()[self.start] {
            MarkEvent::NodeStart { parent, .. } => *parent = m.position,
            _ => unreachable!(),
        }
        p.get_events().push(MarkEvent::Trivia);
        m
    }

    /// Like [`CompleteMarker::precede`], but safe to call on an empty marker: if the completed
    /// node produced no events there is nothing to re-parent, so the newly created marker is
    /// undone and the empty marker is returned unchanged.
    pub fn precede_or_noop<P: MarkerEventContainer>(
        self,
        p: &mut P,
        kind: CppSyntaxKind,
    ) -> CompleteMarker {
        if self.is_empty() {
            return self;
        }

        let m = self.precede(p, kind);
        CompleteMarker {
            start: m.position,
            kind,
        }
    }

    pub fn empty() -> Self {
        CompleteMarker {
            start: EMPTY_NODE_POSITION,
            kind: CppSyntaxKind::None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.start == EMPTY_NODE_POSITION || self.kind == CppSyntaxKind::None
    }

    pub fn start_position(&self) -> usize {
        self.start
    }
}
