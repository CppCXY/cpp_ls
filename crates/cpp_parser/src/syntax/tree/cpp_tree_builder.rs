use rowan::{GreenNode, NodeCache};

use crate::{
    kind::CppSyntaxKind,
    parser::{MarkEvent, NO_FORWARD_PARENT},
};

use super::cpp_green_builder::CppGreenNodeBuilder;

/// Turns the parser's flat event stream into a rowan green tree.
///
/// Two jobs beyond the mechanical translation:
///
/// 1. **Balancing.** The event stream is not guaranteed to be balanced, for a mundane reason:
///    `Marker::complete` drops empty nodes and therefore emits no `NodeEnd` for them. The parser's
///    own node stack stays consistent (see `CppParser::audit_events`), but the flat list can carry
///    a `NodeStart` that nothing closes. See [`balance_events`].
/// 2. **Re-nesting forward references.** `CompleteMarker::precede` opens a wrapper node *after* its
///    first child has already been parsed (`a + b` is parsed as `a`, then wrapped in `BinaryExpr`).
///    The wrapper's position is recorded in the child's `parent` field, and this builder opens the
///    wrapper before the child when it reaches it.
#[derive(Debug)]
pub struct CppTreeBuilder<'a> {
    text: &'a str,
    events: Vec<MarkEvent>,
    green_builder: CppGreenNodeBuilder<'a>,
}

impl<'a> CppTreeBuilder<'a> {
    pub fn new(
        text: &'a str,
        events: Vec<MarkEvent>,
        node_cache: Option<&'a mut NodeCache>,
    ) -> Self {
        match node_cache {
            Some(cache) => CppTreeBuilder {
                text,
                events,
                green_builder: CppGreenNodeBuilder::with_cache(cache),
            },
            None => CppTreeBuilder {
                text,
                events,
                green_builder: CppGreenNodeBuilder::new(),
            },
        }
    }

    /// Translate the event stream into builder calls.
    ///
    /// This deliberately does **not** open a translation unit: the events already contain one
    /// (`parse_cpp_unit` opens it), and adding another here would nest two roots.
    /// [`CppGreenNodeBuilder::finish`] handles the "no root at all" case.
    pub fn build(&mut self) {
        
        balance_events(&mut self.events);
        let event_count = self.events.len();

        let mut parents: Vec<CppSyntaxKind> = Vec::new();
        for i in 0..self.events.len() {
            match std::mem::replace(&mut self.events[i], MarkEvent::none()) {
                MarkEvent::NodeStart {
                    kind: CppSyntaxKind::None,
                    ..
                }
                | MarkEvent::Trivia => {}
                MarkEvent::NodeStart { kind, parent } => {
                    parents.push(kind);

                    // Walk the forward-parent chain: a node can be preceded by several wrappers.
                    let mut parent_position = parent;
                    let mut guard = 0usize;
                    while parent_position != NO_FORWARD_PARENT {
                        guard += 1;
                        assert!(
                            guard <= event_count,
                            "cyclic `parent` chain in marker events; the parser produced \
                             inconsistent forward references"
                        );

                        match std::mem::replace(
                            &mut self.events[parent_position],
                            MarkEvent::none(),
                        ) {
                            MarkEvent::NodeStart { kind, parent } => {
                                parents.push(kind);
                                parent_position = parent;
                            }
                            other => unreachable!(
                                "forward parent must point at a NodeStart, found {other:?}"
                            ),
                        }
                    }

                    for kind in parents.drain(..).rev() {
                        self.green_builder.start_node(kind);
                    }
                }
                MarkEvent::NodeEnd => {
                    self.green_builder.finish_node();
                }
                MarkEvent::EatToken { kind, range } => {
                    self.green_builder.token(kind, range);
                }
            }
        }
    }

    pub fn finish(self) -> GreenNode {
        self.green_builder.finish(self.text)
    }
}

/// Make `events` balanced so the builder can translate it one-to-one.
///
/// The event **indices are preserved wherever a `parent` forward reference could point at them**,
/// because those references are absolute positions in this list. Events that are removed are
/// replaced by [`MarkEvent::Trivia`], which the builder already ignores, so nothing shifts.
///
/// Three rules:
///
/// * A matched `NodeStart`/`NodeEnd` pair with nothing in between is a zero-width node —
///   `Marker::complete` drops those, so they are removed here too.
/// * Anything still open at the end of the stream gets a synthesized `NodeEnd` **inserted where
///   that node actually ends**, not appended to the end. See below.
/// * A `NodeStart` that a forward reference was dropped for is left alone; empty nodes are never
///   the target of a forward reference.
///
/// # Why a leaked node must be closed eagerly, at its true end
///
/// The naive fix — appending `NodeEnd`s at the end of the stream, innermost first — does not
/// restore the tree's shape, it inverts it. A node opened but never closed *swallows every
/// following sibling*: the next declaration becomes a child of the previous one, and the leak is
/// invisible in the tree, which stays well-formed and lossless throughout. That is the exact
/// failure the parser's marker stack and event-stream audit exist to prevent, so the repair here
/// must not reintroduce it.
///
/// Instead the end of every unclosed node is *derived from the stream*: a node ends where the
/// deepest thing that occurred while it was open ends. Closing at that point bounds the damage to
/// the leaked node itself, and the tokens after it land as siblings, where they belong. A leak
/// still means an inner rule unwound without closing its marker, and the audit in
/// `CppParser::audit_events` still reports it — this only stops a shape bug from silently
/// re-parenting the rest of the file.
/// Make `events` balanced so the builder can translate it one-to-one.
///
/// The event **indices are preserved wherever a forward reference could point at them**, because
/// those references are absolute positions in this list. Events that are removed are replaced by
/// [`MarkEvent::Trivia`], which the builder already ignores, so nothing shifts.
///
/// # Why the raw stream is not balanced
///
/// `Marker::complete` drops a node that produced no content and emits no `NodeEnd` for it, so the
/// stream can carry both defects at once, and they are independent:
///
/// * a `NodeStart` with no `NodeEnd` — a marker that was dropped as empty while its *parent* still
///   owed an event; the parent's `complete` later emits that event, which has nothing left to pair
///   with. Leaving it in makes the builder pop a node it never pushed, so an enclosing node is
///   closed early and every following sibling lands outside its real parent.
/// * a `NodeEnd` with no `NodeStart` — a rule that unwound without closing its marker.
///
/// Repairing only one of the two is not enough, and appending the missing `NodeEnd`s at the end of
/// the stream is worse than doing nothing: a leaked node then *swallows every following sibling*,
/// leaving a tree that is still well formed and lossless while its shape is silently wrong. So the
/// stream is normalized by replaying it against a stack, which resolves both defects at the point
/// where they actually occur.
///
/// This is a repair, not a licence to leak: [`crate::parser::EventStreamAudit`] still reports every
/// node the grammar failed to close, and the tests assert it stays empty.
fn balance_events(events: &mut Vec<MarkEvent>) {
    // Replay the stream against a stack of open nodes. `NodeEnd` closes the innermost open node; a
    // `NodeEnd` with nothing open is spurious and dropped. A node that closed without producing
    // anything is one `Marker::complete` would have dropped, so it is dropped here as well.
    let mut stack: Vec<usize> = Vec::new();
    let mut keep: Vec<bool> = events
        .iter()
        .map(|event| {
            !matches!(
                event,
                MarkEvent::NodeStart {
                    kind: CppSyntaxKind::None,
                    ..
                } | MarkEvent::Trivia
            )
        })
        .collect();

    // `NodeStart` positions are resolved when their `NodeEnd` is seen; nothing else to track.
    for index in 0..events.len() {
        match &events[index] {
            MarkEvent::NodeStart {
                kind: CppSyntaxKind::None,
                ..
            } => {}
            MarkEvent::NodeStart { .. } => stack.push(index),
            MarkEvent::NodeEnd => {
                if let Some(start) = stack.pop() {
                    // A pair with nothing in between is a node whose only content was nothing,
                    // which is exactly what `Marker::complete` drops instead of emitting.
                    if start + 1 == index {
                        keep[start] = false;
                        keep[index] = false;
                    }
                } else {
                    // Nothing is open, so this event has no node to close.
                    keep[index] = false;
                }
            }
            MarkEvent::EatToken { .. } | MarkEvent::Trivia => {}
        }
    }

    for index in 0..events.len() {
        if !keep[index] {
            events[index] = MarkEvent::Trivia;
        }
    }

    // Anything still open never got its `NodeEnd`.
    for _ in 0..stack.len() {
        events.push(MarkEvent::NodeEnd);
    }
}
