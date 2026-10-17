#![allow(dead_code)]
//! **The projection's reuse ratio** — the one number `docs/walk-redesign.md` says stands between the sharing design
//! and abandoning it.
//!
//! # What is being counted, and why this key
//!
//! The design's first key was the **whole environment**: the set of names in force when a file is entered. Measured
//! (`docs/walk-redesign.md` §9, §10): it is a key that never repeats — every root defines its own macros before it
//! reaches a shared header, so 99,238 entries fell into 67,554 pairs and the cache would have bought **1.5×**.
//!
//! The refinement is a *projection*: a file's conditions do not depend on the whole set in force, only on the **names
//! those conditions mention**.
//!
//! ```text
//! #if defined(_STD_BEGIN) || _HAS_CXX20      depends on two names, and on nothing the root did
//! ```
//!
//! A root that defines `MY_FEATURE` before including `<string>` changes the environment and not this projection, so
//! the entry still hits. That is the same idea clangd uses when it hashes what a preamble *uses*.
//!
//! # What is counted, exactly
//!
//! * **entries** — every file the walk enters, which is the work that would be skipped on a hit;
//! * **distinct (file, projection)** — the entries that could not hit, which is the work that would remain.
//!
//! An entry's key is the file plus, for each name its conditions mention, **whether it is defined at that moment**.
//! Definedness and not the definition, because that is what the walk itself carries: `UnitState`'s own note says the
//! walk answers with definedness and not with values.
//!
//! # The names, read statically
//!
//! From the file's own text, by the preprocessor's own syntax: the identifiers on its `#if` / `#ifdef` / `#ifndef` /
//! `#elif` lines. No walk, no summary, no coordinates — which is what makes the key something a cache could compute
//! before deciding whether to walk at all.
//!
//! Stated in the safe direction: the scan is slightly **wider** than the conditions' true dependencies (it takes every
//! identifier on the line, including keywords and macro *bodies*), and a wider key makes **fewer** hits. A ratio
//! measured this way is therefore a floor, not an estimate.
//!
//! # Why this is a probe and not a cache
//!
//! `docs/walk-redesign.md` §8 says the count comes first, and §10 says what skipping it cost. So this counts and
//! prints, holds no answers, and is deleted once the number is in — including the counters, because a cache that
//! cannot hit is worse than no cache: it *looks* like the fix.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Is the census on? Read **once** — a probe that reads its environment per entry is a probe that measures itself.
fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CPPLS_WALK_CENSUS").is_some())
}

static ENTRIES: AtomicU64 = AtomicU64::new(0);
/// How many entries were answered by **splicing** a shared contribution instead of walking.
static SPLICED: AtomicU64 = AtomicU64::new(0);
/// How many entries were actually walked — the number the sharing is supposed to drive toward the distinct count.
static FRESH: AtomicU64 = AtomicU64::new(0);
static UNITS: AtomicU64 = AtomicU64::new(0);

fn distinct() -> &'static Mutex<HashSet<u64>> {
    static SEEN: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashSet::new()))
}

/// **One file entered** — count it, and print the running totals.
///
/// `definitions` is the walk's own table of names in force: a name is "defined" when the table holds it. The entries
/// are what a cache would skip; the distinct keys are what it would still walk.
pub(crate) fn entered(
    path: &std::path::Path,
    source: &str,
    defined: impl Fn(&str) -> bool,
) {
    if !enabled() {
        return;
    }

    let digests = projection(source);
    let mut key = self::hash(path.to_string_lossy().as_bytes());
    for (name, hash) in &digests {
        // Definedness, and only definedness: a `#define X 1` and a `#define X 2` are the same answer to every
        // condition that asks about `X`.
        key = key.rotate_left(5) ^ hash ^ if defined(name.as_str()) { 0x9e37 } else { 0 };
    }

    ENTRIES.fetch_add(1, Ordering::Relaxed);
    let mut seen = distinct().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    seen.insert(key);
    let distinct_now = seen.len() as u64;
    let entries = ENTRIES.load(Ordering::Relaxed);

    // Printed on **every** entry rather than only at the end: the library has no "the load is over" hook, the last
    // line has the totals, and 1122 lines is nothing beside what they measure.
    let spliced = SPLICED.load(Ordering::Relaxed);
    let fresh = FRESH.load(Ordering::Relaxed);
    eprintln!(
        "census: entries {entries} distinct {distinct_now} ratio {:.2} | spliced {spliced} fresh {fresh}",
        entries as f64 / distinct_now.max(1) as f64
    );
}

/// **One entry answered by a splice** — see `index::walk_cache`.
pub(crate) fn a_splice() {
    if enabled() {
        SPLICED.fetch_add(1, Ordering::Relaxed);
    }
}

/// **One entry walked for real**, so that the two can be read against each other in the census line.
pub(crate) fn a_fresh_walk() {
    if enabled() {
        FRESH.fetch_add(1, Ordering::Relaxed);
    }
}

/// **Announce a unit**, so the totals can be read against how many walks produced them.
pub(crate) fn unit() {
    if enabled() {
        let units = UNITS.fetch_add(1, Ordering::Relaxed) + 1;
        if units % 200 == 0 {
            let entries = ENTRIES.load(Ordering::Relaxed);
            let distinct_now = distinct().lock().map(|seen| seen.len()).unwrap_or(0) as u64;
            eprintln!(
                "census: after {units} unit(s), entries {entries} distinct {distinct_now} ratio {:.2}",
                entries as f64 / distinct_now.max(1) as f64
            );
        }
    }
}

/// **The names this file's conditions mention**, deduplicated, with a hash of each.
///
/// By the preprocessor's syntax rather than by a parse: a line whose first token is `#` and whose next is `if`,
/// `ifdef`, `ifndef` or `elif`. Identifiers are taken as runs of `[A-Za-z0-9_]` that start with a letter or `_` — a
/// narrower rule would miss a name, and a missing name makes the key **coarser** than the truth, which is the
/// direction that would flatter the ratio.
fn projection(source: &str) -> Vec<(String, u64)> {
    let mut names: Vec<(String, u64)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(after_hash) = trimmed.strip_prefix('#') else {
            continue;
        };
        let after_hash = after_hash.trim_start();
        let directive: String = after_hash
            .chars()
            .take_while(|character| character.is_ascii_alphabetic())
            .collect();
        if !matches!(directive.as_str(), "if" | "ifdef" | "ifndef" | "elif") {
            continue;
        }

        let words = after_hash[directive.len()..].chars();
        let mut word = String::new();
        let flush = |word: &mut String, names: &mut Vec<(String, u64)>, seen: &mut HashSet<&str>| {
            if word.is_empty() {
                return;
            }
            // A name that only appears once per directive still belongs in the key; the set is what matters.
            if seen.insert(Box::leak(word.clone().into_boxed_str())) {
                let hash = self::hash(word.as_bytes());
                names.push((word.clone(), hash));
            }
            word.clear();
        };
        for character in words {
            if character.is_alphanumeric() || character == '_' {
                word.push(character);
            } else {
                flush(&mut word, &mut names, &mut seen);
            }
        }
        flush(&mut word, &mut names, &mut seen);
    }

    names
}

/// A 64-bit hash with no dependency and no state: FNV-1a, which is the same everywhere and therefore the same across
/// the two runs a ratio is compared between.
pub(crate) fn hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
/// **The projection's digest for a file, against the names in force** — the key `index::walk_cache` files a
/// contribution under.
///
/// The same scanner §11's ratio was measured with, so that what the cache keys on and what the ratio describes are one
/// thing rather than two: the identifiers on the file's own conditional lines, each contributing *definedness* and
/// nothing else. `defined` is the unit's own table, asked per name.
// **Wired in by the splice** — `docs/walk-redesign.md` §11.2 step 4. Unused until then, and allowed rather than
// removed: it is the second half of the cache key, and its test is the ratio in §11.
#[allow(dead_code)]
pub(crate) fn projection_digest(source: &str, defined: impl Fn(&str) -> bool) -> u64 {
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, hash) in projection(source) {
        let value = hash ^ if defined(&name) { 0x9e37_79b9_7f4a_7c15 } else { 0 };
        digest = digest.rotate_left(5) ^ value;
    }
    digest
}



