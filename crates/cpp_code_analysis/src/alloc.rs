//! **A byte counter a reader can turn on**, so a stage that allocates can be named without a profiler.
//!
//! The server installs an allocator that reports every request here; the counter is off unless
//! `CPPLS_TRACE_ALLOCS` is in the environment, and the switch is one relaxed load per allocation.
//!
//! # Why this lives in the engine and not in the server
//!
//! Because the question is about the engine's own phases: a wave preparation that asks for **663 MB** while it has
//! nothing to prepare (`test_cpp/src/containers.cpp`, one open file, no requests) is a question about
//! `Session::prepare_wave`'s three calls, and they are private to this crate. The server's side of the same counter
//! is [`cpp_ls::allocated_bytes`], which forwards here.
//!
//! It is an instrument, not a statistic: nothing reads it in an answer, and with the variable unset it costs the
//! branch.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Bytes this process has asked the allocator for since the counter was turned on.
static BYTES: AtomicUsize = AtomicUsize::new(0);

/// Whether [`note`] should count.
static COUNTING: AtomicBool = AtomicBool::new(false);

/// **Turn the counter on from `CPPLS_TRACE_ALLOCS`** — called once, while the server starts.
pub fn turn_on_from_the_environment() {
    if std::env::var_os("CPPLS_TRACE_ALLOCS").is_some() {
        COUNTING.store(true, Ordering::Relaxed);
    }
}

/// One allocation, as the installed allocator reports it.
#[inline]
pub fn note(bytes: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// The counter's current value — monotone, so a stage measures a difference.
#[inline]
pub fn bytes() -> usize {
    BYTES.load(Ordering::Relaxed)
}

/// Whether a run asked for the counter — what a stage's `eprintln!` is gated on.
#[inline]
pub fn counting() -> bool {
    COUNTING.load(Ordering::Relaxed)
}

thread_local! {
    /// **This thread's share of the counter** — the question "did the pump's own stage spend it, or another
    /// thread?" is not answerable from a process-wide total.
    static HERE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One allocation, as the installed allocator reports it, **per thread as well as in total**.
#[inline]
pub fn note_here(bytes: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        BYTES.fetch_add(bytes, Ordering::Relaxed);
        HERE.with(|cell| cell.set(cell.get() + bytes));
    }
}

/// What this thread has asked for.
#[inline]
pub fn bytes_here() -> usize {
    HERE.with(std::cell::Cell::get)
}
