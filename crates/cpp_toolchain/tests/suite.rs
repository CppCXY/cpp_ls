//! This crate's integration tests, as **one** test target.
//!
//! One binary per `tests/*.rs` costs one **link** each, and the link is what a gate spends its time on: measured in
//! this workspace, `cpp_code_analysis` linked 142 test binaries at ~2 s apiece (294 s) while the tests themselves ran
//! in 4.85 s. The files live in `tests/suite/` — a subdirectory, which cargo does not treat as a target — and this
//! file declares each of them as a module, so they keep their own names and helpers and only the linking changes.

#[path = "suite/expansion_budget.rs"]
mod expansion_budget;
#[path = "suite/preprocessor_against_the_compiler.rs"]
mod preprocessor_against_the_compiler;
