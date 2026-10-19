#![allow(dead_code)]
//! **The projection's reuse ratio, and the memo that makes it cheap** — the number `docs/walk-redesign.md` says stands
//! between the sharing design and abandoning it, and the key it is measured with.
//!
//! # The key, and why it is a projection
//!
//! The design's first key was the **whole environment**: the set of names in force when a file is entered. Measured
//! (`docs/walk-redesign.md` §9, §10): it never repeats — every root defines its own macros before it reaches a shared
//! header, so 99,238 entries fell into 67,554 pairs and a cache would have bought **1.5×**.
//!
//! The refinement is a projection: a file's conditions depend only on the **names those conditions mention**.
//!
//! ```text
//! #if defined(_STD_BEGIN) || _HAS_CXX20      depends on two names, and on nothing the root did
//! ```
//!
//! A root that defines `MY_FEATURE` before including `<string>` changes the environment and **not** this projection, so
//! the entry still hits. Measured on EmmyLuaCodeStyle: **91,290 entries into 2,452 distinct pairs — 37×** (§11).
//!
//! # The names come from the file's own text
//!
//! Not from a walk and not from a summary: the identifiers on the file's `#if` / `#ifdef` / `#ifndef` / `#elif` lines,
//! by the preprocessor's own syntax. The scan is deliberately **wider** than the true dependency (every identifier on
//! the line, `defined` included), and a wider key makes fewer hits — so a ratio measured this way is a floor.
//!
//! # Why the memo exists, measured
//!
//! The first version scanned the text on **every entry** and leaked every identifier it saw. With the cache skipping
//! 98.7% of the walks the load still went from **103 s to 159 s**: the key cost more than the work it saved. So the
//! scan is memoized per file behind a fingerprint that is O(1) in the file's size, and is cleared with the
//! contributions — the same edits invalidate both.
//!
//! # The counters
//!
//! `entries` is every file the walk enters; `distinct` is how many could not be skipped; `spliced` is how many were
//! answered from a contribution and `fresh` how many were really walked. A probe reports them (`CPPLS_WALK_CENSUS=1`),
//! and the line is printed on every entry so that "it did not run" is never readable as "it measured zero".

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Is the census on? Read **once** — a probe that reads its environment per entry measures itself.
fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CPPLS_WALK_CENSUS").is_some())
}

static ENTRIES: AtomicU64 = AtomicU64::new(0);
static SPLICED: AtomicU64 = AtomicU64::new(0);
static FRESH: AtomicU64 = AtomicU64::new(0);

fn distinct() -> &'static Mutex<HashSet<u64>> {
    static SEEN: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashSet::new()))
}

thread_local! {
    /// The names each file's conditions mention, keyed by path and a cheap fingerprint — see [`projection_of`].
    static PROJECTIONS: std::cell::RefCell<HashMap<std::path::PathBuf, (u64, Arc<Vec<(Box<str>, u64)>>)>> =
        std::cell::RefCell::new(HashMap::new());
}

/// **One file entered** — count it, and print the running totals.
pub(crate) fn entered(path: &std::path::Path, source: &str, defined: impl Fn(&str) -> bool) {
    if !enabled() {
        return;
    }

    let mut key = hash(path.to_string_lossy().as_bytes());
    for (name, hash) in projection_of(path, source).iter() {
        // Definedness, and only definedness: `#define X 1` and `#define X 2` answer every condition about `X` alike.
        key = key.rotate_left(5) ^ hash ^ if defined(name) { 0x9e37 } else { 0 };
    }

    ENTRIES.fetch_add(1, Ordering::Relaxed);
    let distinct_now = {
        let mut seen = distinct()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        seen.insert(key);
        seen.len() as u64
    };
    let entries = ENTRIES.load(Ordering::Relaxed);
    eprintln!(
        "census: entries {entries} distinct {distinct_now} ratio {:.2} | spliced {} fresh {}",
        entries as f64 / distinct_now.max(1) as f64,
        SPLICED.load(Ordering::Relaxed),
        FRESH.load(Ordering::Relaxed),
    );
}

/// **One entry answered by a splice**.
pub(crate) fn a_splice() {
    if enabled() {
        SPLICED.fetch_add(1, Ordering::Relaxed);
    }
}

/// **One entry walked for real**, so the two can be read against each other.
pub(crate) fn a_fresh_walk() {
    if enabled() {
        FRESH.fetch_add(1, Ordering::Relaxed);
    }
}

/// **The projection's digest against the names in force** — the key `index::walk_cache` files a contribution under.
pub(crate) fn projection_digest(
    path: &std::path::Path,
    source: &str,
    defined: impl Fn(&str) -> bool,
) -> u64 {
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, hash) in projection_of(path, source).iter() {
        let value = hash
            ^ if defined(name) {
                0x9e37_79b9_7f4a_7c15
            } else {
                0
            };
        digest = digest.rotate_left(5) ^ value;
    }
    digest
}

/// **The names a file's conditions mention**, memoized — see the module documentation for the 103 s → 159 s that made
/// the memo necessary.
pub(crate) fn projection_of(path: &std::path::Path, source: &str) -> Arc<Vec<(Box<str>, u64)>> {
    if path.as_os_str().is_empty() {
        return Arc::new(scan(source));
    }

    let fingerprint = fingerprint_of(source);
    let hit = PROJECTIONS.with(|held| {
        held.borrow()
            .get(path)
            .filter(|(known, _)| *known == fingerprint)
            .map(|(_, names)| names.clone())
    });
    if let Some(hit) = hit {
        return hit;
    }

    let names = Arc::new(scan(source));
    PROJECTIONS.with(|held| {
        held.borrow_mut()
            .insert(path.to_path_buf(), (fingerprint, names.clone()));
    });
    names
}

/// **Forget the memo** — called with the contributions, because the same edits invalidate both.
pub(crate) fn forget_projections() {
    PROJECTIONS.with(|held| held.borrow_mut().clear());
}

/// A length-and-edges fingerprint: O(1) in the file's size, and enough to notice a change a *cheap* check can see.
fn fingerprint_of(source: &str) -> u64 {
    let mut hash = source.len() as u64;
    for byte in source.as_bytes().chunks(64).take(4).flatten() {
        hash = hash.rotate_left(5) ^ u64::from(*byte);
    }
    hash
}

/// **The scan itself**: the identifiers on a file's conditional lines, deduplicated, each with its hash.
///
/// By the preprocessor's syntax rather than by a parse — a line whose first token is `#` and whose next is `if`,
/// `ifdef`, `ifndef` or `elif`.
fn scan(source: &str) -> Vec<(Box<str>, u64)> {
    let mut names: Vec<(Box<str>, u64)> = Vec::new();
    let mut seen: HashSet<Box<str>> = HashSet::new();

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

        let mut word = String::new();
        let flush =
            |word: &mut String, names: &mut Vec<(Box<str>, u64)>, seen: &mut HashSet<Box<str>>| {
                if word.is_empty() {
                    return;
                }
                if seen.insert(Box::from(&**word)) {
                    names.push((Box::from(&**word), hash(word.as_bytes())));
                }
                word.clear();
            };
        for character in after_hash[directive.len()..].chars() {
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

/// FNV-1a: no dependency, no state, and the same everywhere — so two runs are comparable.
pub(crate) fn hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
