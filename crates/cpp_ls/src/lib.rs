#[macro_use]
extern crate rust_i18n;

// **The text this server says in its own voice** — the keyword and Doxygen vocabularies in `locales/en.yml`, and
// whatever a future message needs. Embedded at build time, so a released binary needs no files beside it.
rust_i18n::i18n!("./locales", fallback = "en");

pub mod cmd_args;
mod context;
pub mod handlers;
mod logger;
pub mod probes;
mod server;
mod util;

pub use clap::Parser;
pub use cmd_args::*;
pub use server::{AsyncConnection, ExitError, run_ls};

/// **One allocation**, reported by the binary's allocator — and the *only* counter, because the server's answers are
/// about the engine's phases and the engine owns the counting. See [`cpp_code_analysis::alloc`].
#[inline]
pub fn note_allocation(bytes: usize) {
    cpp_code_analysis::alloc::note(bytes);
}

/// **One free**, reported by the binary's allocator. Without it [`live_bytes`] would be a running total and not a
/// measurement — which is exactly what every memory claim in this repository used to be.
#[inline]
pub fn note_release(bytes: usize) {
    cpp_code_analysis::alloc::released(bytes);
}

/// **One reallocation**: a release of the old size and an allocation of the new one.
#[inline]
pub fn note_reallocation(old: usize, new: usize) {
    cpp_code_analysis::alloc::replaced(old, new);
}

/// **Bytes this process is holding right now** — the number a machine runs out of.
#[inline]
pub fn live_bytes() -> usize {
    cpp_code_analysis::alloc::live()
}

/// The high-water mark of [`live_bytes`].
#[inline]
pub fn peak_bytes() -> usize {
    cpp_code_analysis::alloc::peak()
}

/// Every byte ever asked for, which is churn rather than memory — see [`cpp_code_analysis::alloc`].
#[inline]
pub fn allocated_bytes() -> usize {
    cpp_code_analysis::alloc::total()
}
