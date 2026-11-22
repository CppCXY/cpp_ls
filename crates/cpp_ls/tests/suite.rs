//! This crate's integration tests, as **one** test target.
//!
//! One binary per `tests/*.rs` costs one **link** each, and the link is what a gate spends its time on: measured in
//! this workspace, `cpp_code_analysis` linked 142 test binaries at ~2 s apiece (294 s) while the tests themselves ran
//! in 4.85 s. The files live in `tests/suite/` — a subdirectory, which cargo does not treat as a target — and this
//! file declares each of them as a module, so they keep their own names and helpers and only the linking changes.

#[path = "suite/common.rs"]
mod common;
#[path = "suite/a_header_language.rs"]
mod a_header_language;
#[path = "suite/diagnostics_stable.rs"]
mod diagnostics_stable;
#[path = "suite/formatter_choice.rs"]
mod formatter_choice;
#[path = "suite/handshake.rs"]
mod handshake;
#[path = "suite/hover_attributes.rs"]
mod hover_attributes;
#[path = "suite/hover_layout.rs"]
mod hover_layout;
#[path = "suite/idle.rs"]
mod idle;
#[path = "suite/implementation_declaration.rs"]
mod implementation_declaration;
#[path = "suite/latency.rs"]
mod latency;
#[path = "suite/lsp_status.rs"]
mod lsp_status;
#[path = "suite/module_implementation.rs"]
mod module_implementation;
#[path = "suite/progress.rs"]
mod progress;
#[path = "suite/real_project.rs"]
mod real_project;
#[path = "suite/reported_hover.rs"]
mod reported_hover;
#[path = "suite/watch_rescan.rs"]
mod watch_rescan;
