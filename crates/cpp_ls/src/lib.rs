#[macro_use]
extern crate rust_i18n;

// **The text this server says in its own voice** — the keyword and Doxygen vocabularies in `locales/en.yml`, and
// whatever a future message needs. Embedded at build time, so a released binary needs no files beside it.
rust_i18n::i18n!("./locales", fallback = "en");

pub mod cmd_args;
mod context;
pub mod handlers;
mod logger;
mod server;
mod util;

pub use clap::Parser;
pub use cmd_args::*;
pub use server::{AsyncConnection, ExitError, run_ls};

/// **Bytes this process has asked the allocator for**, when the server was built with the counter on.
///
/// A diagnostic, not a statistic: the pump is a loop of stages that each have a name, and a round that allocates
/// hundreds of megabytes with nothing to do is a question about *which* stage. Read by the pump under
/// `CPPLS_TRACE_ALLOCS`; the binary's allocator is what increments it.
pub static ALLOCATED_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Whether the allocator should count — set once from `CPPLS_TRACE_ALLOCS`, so a normal run pays one relaxed
/// load per allocation and nothing else.
pub static COUNT_ALLOCATIONS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// **One allocation**, reported by the binary's allocator.
#[inline]
pub fn note_allocation(bytes: usize) {
    if COUNT_ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed) {
        ALLOCATED_BYTES.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The counter's current value.
#[inline]
pub fn allocated_bytes() -> usize {
    ALLOCATED_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}
