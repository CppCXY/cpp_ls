#![allow(dead_code)]
//! **The sharing cache on disk** — §8 step 4, "so a restart starts warm".
//!
//! # What is written, and what is not
//!
//! A contribution is one file's own work: its frame, the events it wrote, the include edges it took with the key of
//! what was walked there, the `#define`s it decided are not compiled, the conditional bodies that came out in force, and
//! two counts. That is all plain data and all of it goes on disk.
//!
//! **The ordered state changes do not.** Every one of them is implied by the events — a definition is an event with a
//! body, an `#undef` one without — and a contribution that carried them would carry an `Arc<MacroDef>`, which is the
//! walk's own runtime object and not this format's business. The splice rebuilds them, because the splice is what has a
//! unit's tables and its `look_up`. That is a saving on disk and one less thing that can disagree with the events.
//!
//! # The format
//!
//! ```text
//! MAGIC (8) │ count (u32) │ entry × count
//! entry = key (u64) │ file (bytes) │ visit_once (u8)
//!       │ events (u32 + events) │ edges (u32 + (offset u64, key u64) each)
//!       │ dead (u32 + u64 each) │ bodies (u32 + each) │ conditional_facts (u64) │ facts_in_force (u64)
//! ```
//!
//! `bytes` is a `u32` length and then the bytes. An optional field is `u32::MAX` for "none" and a length otherwise,
//! which is the same convention `summary_codec` uses, so a reader that has seen one has seen both.
//!
//! # Why it is not part of `summary_codec`
//!
//! That file's contract is "the summary and the timeline, keyed by contents". This is a cache with its own invalidation
//! (a key that carries the file's fingerprint and its projection) and its own lifecycle (it is cleared whenever a
//! summary is dropped). Two lifetimes in one file is how a cache ends up reading its neighbour's bytes.

use crate::index::walk_cache::{ConditionalBody, Contribution};
use crate::summary::TuEvent;

/// A magic number, so that reading the wrong file is a loud refusal rather than a plausible cache.
const MAGIC: &[u8; 8] = b"CPPLSWS1";

/// **Write a cache** — the entries, in whatever order the caller has them.
pub(crate) fn encode(entries: &[(u64, Contribution)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());

    for (key, contribution) in entries {
        out.extend_from_slice(&key.to_le_bytes());
        push_bytes(&mut out, contribution.file.to_string_lossy().as_bytes());
        out.push(u8::from(contribution.visit_once));

        out.extend_from_slice(&(contribution.events.len() as u32).to_le_bytes());
        for event in &contribution.events {
            push_bytes(&mut out, event.name.as_bytes());
            out.extend_from_slice(&(event.at as u64).to_le_bytes());
            out.push(u8::from(event.unconditional));
            out.push(match event.function_like {
                None => 2,
                Some(false) => 0,
                Some(true) => 1,
            });
            push_optional(&mut out, event.body_text.as_deref().map(str::as_bytes));
            push_optional(&mut out, event.parameters.as_deref().map(str::as_bytes));
            match event.body_range {
                None => out.push(0),
                Some(range) => {
                    out.push(1);
                    out.extend_from_slice(&(range.start_offset as u64).to_le_bytes());
                    out.extend_from_slice(&(range.length as u64).to_le_bytes());
                }
            }
        }

        out.extend_from_slice(&(contribution.edges.len() as u32).to_le_bytes());
        for (position, child) in &contribution.edges {
            out.extend_from_slice(&(*position as u64).to_le_bytes());
            out.extend_from_slice(&child.to_le_bytes());
        }

        out.extend_from_slice(&(contribution.dead.len() as u32).to_le_bytes());
        for offset in &contribution.dead {
            out.extend_from_slice(&(*offset as u64).to_le_bytes());
        }

        out.extend_from_slice(&(contribution.bodies.len() as u32).to_le_bytes());
        for body in &contribution.bodies {
            push_bytes(&mut out, body.name.as_bytes());
            out.push(u8::from(body.function_like));
            push_optional(&mut out, body.parameters.as_deref().map(str::as_bytes));
            push_bytes(&mut out, body.body.as_bytes());
        }

        out.extend_from_slice(&(contribution.conditional_facts as u64).to_le_bytes());
        out.extend_from_slice(&(contribution.facts_in_force as u64).to_le_bytes());
    }

    out
}

/// **Read a cache** — `None` for anything that is not one, which is simply a cold start.
///
/// Every length is checked against what is left, so a truncated file is refused rather than half-applied: a cache that
/// half-loads would answer some units from a stale contribution and some from a walk, which is the one failure mode
/// this crate refuses.
pub(crate) fn decode(bytes: &[u8]) -> Option<Vec<(u64, Contribution)>> {
    if bytes.len() < MAGIC.len() + 4 || &bytes[..MAGIC.len()] != MAGIC {
        return None;
    }
    let mut at = MAGIC.len();
    let count = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
    at += 4;

    let mut entries = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        let key = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?);
        at += 8;

        let (file, next) = take_bytes(bytes, at)?;
        at = next;
        let mut contribution = Contribution {
            file: std::path::PathBuf::from(std::str::from_utf8(file).ok()?),
            visit_once: *bytes.get(at)? != 0,
            ..Contribution::default()
        };
        at += 1;

        let events = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        for _ in 0..events {
            let (name, next) = take_bytes(bytes, at)?;
            at = next;
            let offset = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?) as usize;
            at += 8;
            let unconditional = *bytes.get(at)? != 0;
            let function_like = match *bytes.get(at + 1)? {
                2 => None,
                flag => Some(flag == 1),
            };
            at += 2;
            let (body_text, next) = take_optional(bytes, at)?;
            at = next;
            let (parameters, next) = take_optional(bytes, at)?;
            at = next;
            let body_range = match *bytes.get(at)? {
                0 => {
                    at += 1;
                    None
                }
                _ => {
                    let start =
                        u64::from_le_bytes(bytes.get(at + 1..at + 9)?.try_into().ok()?) as usize;
                    let length =
                        u64::from_le_bytes(bytes.get(at + 9..at + 17)?.try_into().ok()?) as usize;
                    at += 17;
                    Some(cpp_parser::SourceRange {
                        start_offset: start,
                        length,
                    })
                }
            };
            contribution.events.push(TuEvent {
                name: std::sync::Arc::from(std::str::from_utf8(name).ok()?),
                function_like,
                // The body's *shape* is the parser's, and a contribution never needs it: the splice puts the events in
                // the timeline and the definitions are rebuilt from the summary the walk already holds.
                body: None,
                body_text: body_text
                    .map(|text| std::sync::Arc::from(std::str::from_utf8(text).unwrap_or(""))),
                parameters: parameters
                    .map(|text| std::sync::Arc::from(std::str::from_utf8(text).unwrap_or(""))),
                body_range,
                // A contribution is one file, so every one of its events belongs to frame zero.
                frame: 0,
                at: offset,
                unconditional,
            });
        }

        let edges = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        for _ in 0..edges {
            let position = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?) as usize;
            at += 8;
            let child = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?);
            at += 8;
            contribution.edges.push((position, child));
        }

        let dead = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        for _ in 0..dead {
            contribution
                .dead
                .push(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?) as usize);
            at += 8;
        }

        let bodies = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        for _ in 0..bodies {
            let (name, next) = take_bytes(bytes, at)?;
            at = next;
            let function_like = *bytes.get(at)? != 0;
            at += 1;
            let (parameters, next) = take_optional(bytes, at)?;
            at = next;
            let (body, next) = take_bytes(bytes, at)?;
            at = next;
            contribution.bodies.push(ConditionalBody {
                name: std::str::from_utf8(name).ok()?.into(),
                function_like,
                parameters: parameters
                    .map(|text| String::from_utf8_lossy(text).into_owned().into_boxed_str()),
                body: std::str::from_utf8(body).ok()?.into(),
            });
        }

        contribution.conditional_facts =
            u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?) as usize;
        at += 8;
        contribution.facts_in_force =
            u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?) as usize;
        at += 8;

        entries.push((key, contribution));
    }

    Some(entries)
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn push_optional(out: &mut Vec<u8>, text: Option<&[u8]>) {
    match text {
        None => out.extend_from_slice(&u32::MAX.to_le_bytes()),
        Some(bytes) => push_bytes(out, bytes),
    }
}

fn take_bytes(bytes: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let length = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
    if length == u32::MAX {
        return None;
    }
    let start = at + 4;
    Some((
        bytes.get(start..start + length as usize)?,
        start + length as usize,
    ))
}

fn take_optional(bytes: &[u8], at: usize) -> Option<(Option<&[u8]>, usize)> {
    let length = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
    if length == u32::MAX {
        return Some((None, at + 4));
    }
    let start = at + 4;
    Some((
        Some(bytes.get(start..start + length as usize)?),
        start + length as usize,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::walk_cache::StateChange;

    fn a_contribution() -> Contribution {
        Contribution {
            file: std::path::PathBuf::from("/p/a.h"),
            visit_once: true,
            events: vec![
                TuEvent {
                    name: std::sync::Arc::from("DECLARE"),
                    function_like: Some(true),
                    body: None,
                    body_text: Some(std::sync::Arc::from("struct n##__ { int unused; };")),
                    parameters: Some(std::sync::Arc::from("n")),
                    body_range: Some(cpp_parser::SourceRange {
                        start_offset: 3,
                        length: 4,
                    }),
                    frame: 0,
                    at: 7,
                    unconditional: true,
                },
                TuEvent {
                    name: std::sync::Arc::from("GONE"),
                    function_like: None,
                    body: None,
                    body_text: None,
                    parameters: None,
                    body_range: None,
                    frame: 0,
                    at: 9,
                    unconditional: false,
                },
            ],
            changes: vec![StateChange::Undefined {
                name: Box::from("GONE"),
                file: std::path::PathBuf::from("/p/a.h"),
                at: 9,
            }],
            edges: vec![(11, 0xabc), (13, 0xdef)],
            dead: vec![17],
            bodies: vec![ConditionalBody {
                name: Box::from("Y"),
                function_like: false,
                parameters: Some(Box::from("x")),
                body: Box::from("2"),
            }],
            conditional_facts: 4,
            facts_in_force: 5,
        }
    }

    #[test]
    fn a_cache_survives_the_round_trip() {
        let cache = vec![(9u64, a_contribution())];
        let back = decode(&encode(&cache)).expect("a cache this module wrote must read back");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].0, 9, "the key is the cache's identity");

        let read = &back[0].1;
        let wrote = &cache[0].1;
        assert_eq!(read.file, wrote.file);
        assert_eq!(read.visit_once, wrote.visit_once);
        assert_eq!(read.events.len(), 2);
        assert_eq!(read.events[0].name.as_ref(), "DECLARE");
        assert_eq!(read.events[0].at, 7);
        assert_eq!(read.events[0].function_like, Some(true));
        assert_eq!(
            read.events[0].body_text.as_deref(),
            Some("struct n##__ { int unused; };")
        );
        assert_eq!(read.events[0].parameters.as_deref(), Some("n"));
        assert_eq!(
            read.events[0]
                .body_range
                .map(|range| (range.start_offset, range.length)),
            Some((3, 4))
        );
        assert!(
            !read.events[1].unconditional,
            "an `#undef` is an event with no body"
        );
        assert_eq!(read.edges, wrote.edges);
        assert_eq!(read.dead, wrote.dead);
        assert_eq!(read.bodies.len(), 1);
        assert_eq!(read.bodies[0].name.as_ref(), "Y");
        assert_eq!(read.bodies[0].parameters.as_deref(), Some("x"));
        assert_eq!(read.conditional_facts, 4);
        assert_eq!(read.facts_in_force, 5);
        assert!(
            read.changes.is_empty(),
            "the changes are rebuilt at a splice; a cache that carried them could disagree with the events"
        );
    }

    #[test]
    fn a_foreign_or_truncated_file_is_refused_rather_than_half_applied() {
        assert!(
            decode(b"not a cache file at all").is_none(),
            "the magic is the first check"
        );
        assert!(
            decode(b"CPPLSWS1").is_none(),
            "a magic with no count is not a cache"
        );

        let whole = encode(&[(1, a_contribution())]);
        assert!(
            decode(&whole[..whole.len() - 3]).is_none(),
            "a truncated file is not half a cache"
        );
        assert!(decode(&whole).is_some(), "and the whole one still is");
    }

    #[test]
    fn an_empty_cache_is_a_valid_cache() {
        let back = decode(&encode(&[])).expect("a cold start is not an error");
        assert!(back.is_empty());
    }
}
