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
/// * Anything still open at the end of the stream gets a synthesized `NodeEnd`, appended in
///   innermost-first order. This is the case that actually needs fixing: `complete` emits no
///   `NodeEnd` for a node it drops, so a leaked node leaves a `NodeStart` with no partner.
/// * A `NodeStart` that a forward reference was dropped for is left alone; empty nodes are never
///   the target of a forward reference.
///
/// This is a safety net, not a licence to leak: a leak that survives to the end of the file makes
/// the tree's *shape* wrong (the leaked node swallows its following siblings), which is what the
/// event-stream audit in the tests asserts against.
fn balance_events(events: &mut Vec<MarkEvent>) {
    // First pass: pair every `NodeStart` with its `NodeEnd`, and note which pairs are empty.
    let mut stack: Vec<usize> = Vec::new();
    let mut empty: Vec<(usize, usize)> = Vec::new();

    for (index, event) in events.iter().enumerate() {
        match event {
            MarkEvent::NodeStart {
                kind: CppSyntaxKind::None,
                ..
            } => {}
            MarkEvent::NodeStart { .. } => stack.push(index),
            MarkEvent::NodeEnd => {
                // A pair with nothing between them is a node whose only content was nothing, which
                // is exactly what `Marker::complete` drops instead of emitting.
                if let Some(start) = stack.pop()
                    && start + 1 == index
                {
                    empty.push((start, index));
                }
            }
            _ => {}
        }
    }

    for (start, end) in empty {
        events[start] = MarkEvent::Trivia;
        events[end] = MarkEvent::Trivia;
    }

    // Anything still open never got its `NodeEnd`; close it, innermost (latest) first.
    for _ in 0..stack.len() {
        events.push(MarkEvent::NodeEnd);
    }
}
