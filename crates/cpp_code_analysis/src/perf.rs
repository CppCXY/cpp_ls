//! **Where a read's allocations go**, when the `perf-probe` feature is on.
//!
//! Off by default, and *not compiled* when off — every call site is `#[cfg(feature = "perf-probe")]`, so the build a
//! reader runs has no counter, no atomic and no branch on it. The rule this crate's `no_global_state` test enforces is
//! about state an owner should hold; this is an opt-in instrument, and its state is a build-time choice rather than
//! something a running server carries.
//!
//! The counter itself lives in the allocator the probe installs (`tests/allocation_probe.rs` wraps mimalloc and calls
//! [`note`]). The **marks** are what turn one total into a table: `cook`, `parse`, `index_tree`, `files`, `commit` each
//! record the running counts when they finish, and [`report`] prints the deltas in order.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Allocations reported so far — see [`note`].
pub static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
/// Bytes requested so far.
pub static BYTES: AtomicUsize = AtomicUsize::new(0);

/// **One allocation**, as the probe's wrapping allocator reports it.
#[inline]
pub fn note(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(bytes, Ordering::Relaxed);
}

/// The allocation count so far.
#[inline]
pub fn allocations() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

/// The bytes requested so far.
#[inline]
pub fn bytes() -> usize {
    BYTES.load(Ordering::Relaxed)
}

/// One phase of a read, as it finished.
#[derive(Debug, Clone)]
pub struct Mark {
    pub label: &'static str,
    pub allocations: usize,
    pub bytes: usize,
}

static MARKS: Mutex<Vec<Mark>> = Mutex::new(Vec::new());

/// **A phase of the read has finished.** Recorded in call order; [`report`] turns the sequence into per-phase deltas.
pub fn mark(label: &'static str) {
    if let Ok(mut held) = MARKS.lock() {
        held.push(Mark {
            label,
            allocations: allocations(),
            bytes: bytes(),
        });
    }
}

/// The phases so far, as `(label, allocations, bytes)` **since the previous mark**, summed per label.
///
/// Summed because the passes that matter run once per file: a per-file index calls `build_facts` hundreds of times for
/// one read, and a table with hundreds of `facts loop` rows answers nothing. First-appearance order is kept.
pub fn per_phase() -> Vec<(&'static str, usize, usize)> {
    let Ok(held) = MARKS.lock() else {
        return Vec::new();
    };
    let mut out: Vec<(&'static str, usize, usize)> = Vec::new();
    let mut previous = (0usize, 0usize);
    for mark in held.iter() {
        let allocations = mark.allocations.saturating_sub(previous.0);
        let bytes = mark.bytes.saturating_sub(previous.1);
        previous = (mark.allocations, mark.bytes);
        match out.iter_mut().find(|(label, _, _)| *label == mark.label) {
            Some(entry) => {
                entry.1 += allocations;
                entry.2 += bytes;
            }
            None => out.push((mark.label, allocations, bytes)),
        }
    }
    out
}

/// Forget everything, for a second measurement in the same process.
pub fn reset() {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    if let Ok(mut held) = MARKS.lock() {
        held.clear();
    }
}
