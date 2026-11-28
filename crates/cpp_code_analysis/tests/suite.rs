//! This crate's integration tests, as **one** test target.
//!
//! One binary per `tests/*.rs` costs one **link** each, and the link is what a gate spends its time on: measured in
//! this workspace, `cpp_code_analysis` linked 142 test binaries at ~2 s apiece (294 s) while the tests themselves ran
//! in 4.85 s. The files live in `tests/suite/` — a subdirectory, which cargo does not treat as a target — and this
//! file declares each of them as a module, so they keep their own names and helpers and only the linking changes.

#[path = "suite/alias_members.rs"]
mod alias_members;
#[path = "suite/alias_qualified_members.rs"]
mod alias_qualified_members;
#[path = "suite/allocator_probe.rs"]
mod allocator_probe;
#[path = "suite/api_style.rs"]
mod api_style;
#[path = "suite/c_declarators.rs"]
mod c_declarators;
#[path = "suite/checks.rs"]
mod checks;
#[path = "suite/class_body_continuity.rs"]
mod class_body_continuity;
#[path = "suite/completion.rs"]
mod completion;
#[path = "suite/completion_config.rs"]
mod completion_config;
#[path = "suite/completion_doc.rs"]
mod completion_doc;
#[path = "suite/conditional_definitions.rs"]
mod conditional_definitions;
#[path = "suite/conditional_environment.rs"]
mod conditional_environment;
#[path = "suite/config.rs"]
mod config;
#[path = "suite/constrained_completion.rs"]
mod constrained_completion;
#[path = "suite/contracts.rs"]
mod contracts;
#[path = "suite/cooked.rs"]
mod cooked;
#[path = "suite/definition.rs"]
mod definition;
#[path = "suite/diagnostics.rs"]
mod diagnostics;
#[path = "suite/discovery.rs"]
mod discovery;
#[path = "suite/documentation.rs"]
mod documentation;
#[path = "suite/expand.rs"]
mod expand;
#[path = "suite/file.rs"]
mod file;
#[path = "suite/folding.rs"]
mod folding;
#[path = "suite/format.rs"]
mod format;
#[path = "suite/format_scanner.rs"]
mod format_scanner;
#[path = "suite/generics.rs"]
mod generics;
#[path = "suite/graph.rs"]
mod graph;
#[path = "suite/header_context.rs"]
mod header_context;
#[path = "suite/inactive_regions.rs"]
mod inactive_regions;
#[path = "suite/inlay.rs"]
mod inlay;
#[path = "suite/iterator_element_of_a_const_container.rs"]
mod iterator_element_of_a_const_container;
#[path = "suite/lambda_parameters.rs"]
mod lambda_parameters;
#[path = "suite/language.rs"]
mod language;
#[path = "suite/layout.rs"]
mod layout;
#[path = "suite/library_config.rs"]
mod library_config;
#[path = "suite/lifetime.rs"]
mod lifetime;
#[path = "suite/lua_source_reports.rs"]
mod lua_source_reports;
#[path = "suite/macro_arguments.rs"]
mod macro_arguments;
#[path = "suite/macro_expansion.rs"]
mod macro_expansion;
#[path = "suite/macro_member_types.rs"]
mod macro_member_types;
#[path = "suite/macros.rs"]
mod macros;
#[path = "suite/messages.rs"]
mod messages;
#[path = "suite/modifiers.rs"]
mod modifiers;
#[path = "suite/module_info.rs"]
mod module_info;
#[path = "suite/module_semantics.rs"]
mod module_semantics;
#[path = "suite/modules.rs"]
mod modules;
#[path = "suite/namespace_macros.rs"]
mod namespace_macros;
#[path = "suite/nested_conditional.rs"]
mod nested_conditional;
#[path = "suite/no_global_state.rs"]
mod no_global_state;
#[path = "suite/out_of_class_members.rs"]
mod out_of_class_members;
#[path = "suite/outline.rs"]
mod outline;
#[path = "suite/overload_by_arguments.rs"]
mod overload_by_arguments;
#[path = "suite/preprocess.rs"]
mod preprocess;
#[path = "suite/program_stream_separators.rs"]
mod program_stream_separators;
#[path = "suite/publication_revision.rs"]
mod publication_revision;
#[path = "suite/ranges.rs"]
mod ranges;
#[path = "suite/read_ahead_probe.rs"]
mod read_ahead_probe;
#[path = "suite/refinement.rs"]
mod refinement;
#[path = "suite/reflection.rs"]
mod reflection;
#[path = "suite/rendered_reading.rs"]
mod rendered_reading;
#[path = "suite/rendering_scope.rs"]
mod rendering_scope;
#[path = "suite/reported_defects.rs"]
mod reported_defects;
#[path = "suite/response_files.rs"]
mod response_files;
#[path = "suite/scopes.rs"]
mod scopes;
#[path = "suite/selection.rs"]
mod selection;
#[path = "suite/semantic.rs"]
mod semantic;
#[path = "suite/semantic_real_probe.rs"]
mod semantic_real_probe;
#[path = "suite/signature.rs"]
mod signature;
#[path = "suite/smart_arrow.rs"]
mod smart_arrow;
#[path = "suite/stored_refinement.rs"]
mod stored_refinement;
#[path = "suite/style_checks.rs"]
mod style_checks;
#[path = "suite/subscript_overloads.rs"]
mod subscript_overloads;
#[path = "suite/subscript_qualifiers.rs"]
mod subscript_qualifiers;
#[path = "suite/symbol.rs"]
mod symbol;
#[path = "suite/targets.rs"]
mod targets;
#[path = "suite/toolchain.rs"]
mod toolchain;
#[path = "suite/translation_unit.rs"]
mod translation_unit;
#[path = "suite/types.rs"]
mod types;
#[path = "suite/ue_engine.rs"]
mod ue_engine;
#[path = "suite/ue_lint.rs"]
mod ue_lint;
#[path = "suite/undef_of_an_included_macro.rs"]
mod undef_of_an_included_macro;
#[path = "suite/unit_scope_boundary.rs"]
mod unit_scope_boundary;
