//! This crate's integration tests, as **one** test target.
//!
//! One binary per `tests/*.rs` costs one **link** each, and the link is what a gate spends its time on: measured in
//! this workspace, `cpp_code_analysis` linked 142 test binaries at ~2 s apiece (294 s) while the tests themselves ran
//! in 4.85 s. The files live in `tests/suite/` — a subdirectory, which cargo does not treat as a target — and this
//! file declares each of them as a module, so they keep their own names and helpers and only the linking changes.

#[path = "suite/cases_blocks.rs"]
mod cases_blocks;
#[path = "suite/cases_classes.rs"]
mod cases_classes;
#[path = "suite/cases_comments.rs"]
mod cases_comments;
#[path = "suite/cases_functions.rs"]
mod cases_functions;
#[path = "suite/cases_preprocessor.rs"]
mod cases_preprocessor;
#[path = "suite/cases_reflow.rs"]
mod cases_reflow;
#[path = "suite/cases_short_functions.rs"]
mod cases_short_functions;
#[path = "suite/cases_spacing.rs"]
mod cases_spacing;
#[path = "suite/cases_statements.rs"]
mod cases_statements;
#[path = "suite/cases_tables.rs"]
mod cases_tables;
#[path = "suite/cases_templates.rs"]
mod cases_templates;
#[path = "suite/cases_types.rs"]
mod cases_types;
#[path = "suite/cases_wrapping.rs"]
mod cases_wrapping;
#[path = "suite/layout_reflow.rs"]
mod layout_reflow;
#[path = "suite/options.rs"]
mod options;
