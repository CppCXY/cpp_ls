#[macro_use]
extern crate rust_i18n;

// **The text this server says in its own voice** — the keyword and Doxygen vocabularies in `locales/en.yml`, and
// whatever a future message needs. Embedded at build time, so a released binary needs no files beside it.
rust_i18n::i18n!("./locales", fallback = "en");

pub mod cmd_args;
mod context;
mod handlers;
mod logger;
mod server;
mod util;

pub use clap::Parser;
pub use cmd_args::*;
pub use server::{AsyncConnection, ExitError, run_ls};
