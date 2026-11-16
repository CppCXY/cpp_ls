//! **A byte counter a reader can turn on**, so a stage that allocates can be named without a profiler.
//!
//! The server installs an allocator that reports every request here; the counter is off unless
//! `CPPLS_TRACE_ALLOCS` is in the environment, and the switch is one relaxed load per allocation.
//!
//! # Two numbers, and the one that matters
//!
//! [`total`] counts every byte ever asked for and never subtracts: it answers *"how much churn did this stage
//! cause"*, and it is **not** a memory measurement. [`live`] counts what the process is holding — every
//! allocation adds, every free subtracts — and [`peak`] is its high-water mark. Only the second pair says what a
//! machine runs out of.
//!
//! The distinction is not pedantry. Every memory claim in this codebase was read off the first number, and it
//! produced a story that does not survive the second one: **~380 bytes per token of parse tree** was the churn of a
//! 218 KB header's parse, while the tree it actually held was 80 bytes per token and the whole reading's steady state
//! was 151 (`tests/parse_memory_probe.rs`). A ceiling built on that number was regulating something that was never
//! measured.
//!
//! # Why this lives in the engine and not in the server
//!
//! Because the question is about the engine's own phases: a wave preparation that holds **663 MB** while it has
//! nothing to prepare (`test_cpp/src/containers.cpp`, one open file, no requests) is a question about
//! `Session::prepare_wave`'s three calls, and they are private to this crate. The server's side of the same counter
//! is [`cpp_ls::note_allocation`], which forwards here.
//!
//! It is an instrument, not a statistic: nothing reads it in an answer, and with the variable unset it costs the
//! branch.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Bytes this process has asked the allocator for since the counter was turned on — **cumulative, never reduced**.
static TOTAL: AtomicUsize = AtomicUsize::new(0);

/// Bytes this process is **holding right now** — every allocation adds, every free subtracts.
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// The high-water mark of [`LIVE`].
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Whether [`note`] should count.
static COUNTING: AtomicBool = AtomicBool::new(false);

/// **Turn the counter on from `CPPLS_TRACE_ALLOCS`** — called once, while the server starts.
pub fn turn_on_from_the_environment() {
    // The variable is read by [crate::probes::allocs], once, like every other switch — this is the *state* the
    // counter keeps, so a trace that is gated on it and the counter that fills it cannot disagree.
    if crate::probes::allocs() {
        COUNTING.store(true, Ordering::Relaxed);
    }
}

/// One allocation, as the installed allocator reports it.
#[inline]
pub fn note(bytes: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        TOTAL.fetch_add(bytes, Ordering::Relaxed);
        let now = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
        PEAK.fetch_max(now, Ordering::Relaxed);
    }
}

/// **One free**, as the installed allocator reports it — the half that makes [`live`] a measurement rather than a
/// running total.
#[inline]
pub fn released(bytes: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        LIVE.fetch_sub(bytes, Ordering::Relaxed);
    }
}

/// **One reallocation**, which is a release of the old size and an allocation of the new one.
#[inline]
pub fn replaced(old: usize, new: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        if new >= old {
            let grown = new - old;
            TOTAL.fetch_add(grown, Ordering::Relaxed);
            let now = LIVE.fetch_add(grown, Ordering::Relaxed) + grown;
            PEAK.fetch_max(now, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(old - new, Ordering::Relaxed);
        }
    }
}

/// **The bytes held right now** — the number a machine runs out of.
#[inline]
pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// The high-water mark of [`live`].
#[inline]
pub fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// Every byte ever asked for — churn, not memory. See the module documentation for why the difference matters.
#[inline]
pub fn total() -> usize {
    TOTAL.load(Ordering::Relaxed)
}

/// Whether a run asked for the counter — what a stage's `eprintln!` is gated on.
#[inline]
pub fn counting() -> bool {
    COUNTING.load(Ordering::Relaxed)
}
