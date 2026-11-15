//! The session's tests, one file per question.
//!
//! Split out of `session/tests.rs` without a change to any test.

mod buffers;
mod cache;
mod closure;
mod config;
mod cooked;
mod disk;
mod fixtures;
mod names;
mod programs;
mod types;

pub(crate) use disk::*;
pub(crate) use fixtures::*;
