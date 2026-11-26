//! This crate's integration tests, as **one** test target.
//!
//! One binary per `tests/*.rs` costs one **link** each, and the link is what a gate spends its time on: measured in
//! this workspace, `cpp_code_analysis` linked 142 test binaries at ~2 s apiece (294 s) while the tests themselves ran
//! in 4.85 s. The files live in `tests/suite/` — a subdirectory, which cargo does not treat as a target — and this
//! file declares each of them as a module, so they keep their own names and helpers and only the linking changes.

/// The assertions the parser tests share — one module for the target, so the file is loaded once.
#[path = "suite/common/mod.rs"]
mod common;

#[path = "suite/ascend.rs"]
mod ascend;
#[path = "suite/ast.rs"]
mod ast;
#[path = "suite/c_language.rs"]
mod c_language;
#[path = "suite/concepts.rs"]
mod concepts;
#[path = "suite/contracts.rs"]
mod contracts;
#[path = "suite/cpp11.rs"]
mod cpp11;
#[path = "suite/cpp14.rs"]
mod cpp14;
#[path = "suite/cpp17.rs"]
mod cpp17;
#[path = "suite/cpp20.rs"]
mod cpp20;
#[path = "suite/cpp23.rs"]
mod cpp23;
#[path = "suite/cuda.rs"]
mod cuda;
#[path = "suite/declarator_shapes.rs"]
mod declarator_shapes;
#[path = "suite/designators.rs"]
mod designators;
#[path = "suite/direct_init.rs"]
mod direct_init;
#[path = "suite/doc.rs"]
mod doc;
#[path = "suite/doc_lexer.rs"]
mod doc_lexer;
#[path = "suite/doc_owner.rs"]
mod doc_owner;
#[path = "suite/doc_shape.rs"]
mod doc_shape;
#[path = "suite/docs_ast.rs"]
mod docs_ast;
#[path = "suite/expressions.rs"]
mod expressions;
#[path = "suite/extensions_clang.rs"]
mod extensions_clang;
#[path = "suite/extensions_comprehensive.rs"]
mod extensions_comprehensive;
#[path = "suite/extensions_gnu.rs"]
mod extensions_gnu;
#[path = "suite/extensions_msvc.rs"]
mod extensions_msvc;
#[path = "suite/features.rs"]
mod features;
#[path = "suite/gaps.rs"]
mod gaps;
#[path = "suite/invariants.rs"]
mod invariants;
#[path = "suite/lexer.rs"]
mod lexer;
#[path = "suite/linkage_declarations.rs"]
mod linkage_declarations;
#[path = "suite/lossless.rs"]
mod lossless;
#[path = "suite/macros.rs"]
mod macros;
#[path = "suite/modern.rs"]
mod modern;
#[path = "suite/old_style.rs"]
mod old_style;
#[path = "suite/operators.rs"]
mod operators;
#[path = "suite/reflection.rs"]
mod reflection;
#[path = "suite/smoke.rs"]
mod smoke;
#[path = "suite/symbols.rs"]
mod symbols;
#[path = "suite/token_range.rs"]
mod token_range;
