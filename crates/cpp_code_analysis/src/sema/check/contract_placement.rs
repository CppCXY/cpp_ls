//! **`contract-on-virtual-function`** and **`contract-on-defaulted-function`** — the two contract placements C++26
//! refuses.
//!
//! ```cpp
//! struct Base {
//!     virtual int compute(int x) pre(x > 0);      // error: a virtual function has no contract specifiers
//! };
//!
//! struct Widget {
//!     int size;
//!     Widget() pre(true) = default;               // error: defaulted on its first declaration
//! };
//! ```
//!
//! Both are P2900's own words: a *function-contract-specifier* shall not appear on a virtual function (until C++29),
//! and a function that is defaulted on its **first declaration** shall not have one. The grammar accepts both — the
//! specifier is written in the one position a function declarator owns — so this is what the layer is for: a construct
//! the grammar reads and the language does not.
//!
//! # What is asked here, and what is left alone
//!
//! ```text
//! virtual function + a contract specifier              -> report
//! = default + a contract specifier, first declaration  -> report
//! = default + a contract specifier, later declaration  -> silent (the standard allows it: an out-of-class
//!                                                        `Widget::Widget() = default;` is not the first declaration)
//! any function without a contract specifier            -> silent
//! ```
//!
//! The "first declaration" half is answered from the summary: a class body is one file, so the function's other
//! declarations are the facts with the same `scope` and `name`, and the first one is the one with the smallest offset.
//! A later declaration that is defaulted is legal with contracts, and reporting it would be a diagnostic on correct
//! code — the failure mode this layer is written against.
//!
//! # Why the tree, and not the fact
//!
//! Whether a declaration carries a contract is not a field on `DeclFact`: the summary records what a *query* needs,
//! and a contract is not yet something a query asks. The tree knows exactly — the specifier sequence is a node inside
//! the declarator — so this rule walks the file's own declarators and matches each one's name to the fact it produced.
//! The two ranges are in the file's coordinates (`FileView::file_range_of`), which is what makes the match safe when
//! the declaration reached the tree through a macro.

use super::{Check, CheckContext, DiagnosticCode};

/// **`contract-on-virtual-function` / `contract-on-defaulted-function`** — see the module documentation.
pub struct ContractPlacement;

impl Check for ContractPlacement {
    const CODES: &'static [DiagnosticCode] = &[
        DiagnosticCode::ContractOnVirtualFunction,
        DiagnosticCode::ContractOnDefaultedFunction,
    ];

    fn check(context: &mut CheckContext<'_>) {
        std::fs::write("E:/cpp_ls/target/check-ran.txt", format!("ran with c={}", context.is_c())).ok();
        // **The declarations that carry a contract**, by the name they declare and where that name is written in the
        // *file* — the same coordinates `DeclFact::name_range` uses.
        let mut contracted: Vec<(String, cpp_parser::SourceRange)> = Vec::new();
        for node in context.own_syntax_nodes() {
            if cpp_parser::CppSyntaxKind::from(node.kind())
                != cpp_parser::CppSyntaxKind::ContractSpecifierSeq
            {
                continue;
            }
            // The declarator the specifiers belong to is the nearest one, and the name is the identifier inside it
            // before any parameter list.
            let Some(declarator) = node.ancestors().find(|ancestor| {
                cpp_parser::CppSyntaxKind::from(ancestor.kind())
                    == cpp_parser::CppSyntaxKind::Declarator
            }) else {
                continue;
            };
            let Some(name) = declarator
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| {
                    cpp_parser::CppTokenKind::from(token.kind())
                        == cpp_parser::CppTokenKind::Identifier
                })
            else {
                continue;
            };
            let Some(in_the_file) =
                context.view.file_range_of(cpp_parser::source_range(name.text_range()))
            else {
                continue;
            };
            contracted.push((name.text().to_string(), in_the_file));
        }

        eprintln!("[contract] contracted: {contracted:?}");

        // **What the summary says about those declarations.** A fact is matched by its name range, which is the
        // identifier the tree just gave us.
        let declarations: Vec<(String, crate::DeclModifiers, cpp_parser::SourceRange, Option<String>)> =
            context
                .summary
                .declarations
                .iter()
                .filter(|fact| fact.kind == crate::DeclKind::Function)
                .map(|fact| {
                    (
                        fact.name.clone(),
                        fact.modifiers,
                        fact.name_range,
                        fact.scope.clone(),
                    )
                })
                .collect();

        eprintln!(
            "[contract] facts: {:?}",
            declarations
                .iter()
                .map(|(name, modifiers, range, scope)| (name.clone(), modifiers.bits(), range.start_offset, scope.clone()))
                .collect::<Vec<_>>()
        );
        for (name, name_range) in &contracted {
            let Some((_, modifiers, _, scope)) = declarations
                .iter()
                .find(|(declared, _, range, _)| {
                    declared == name && range.start_offset == name_range.start_offset
                })
            else {
                continue;
            };

            if modifiers.contains(crate::DeclModifiers::VIRTUAL) {
                context.add(
                    DiagnosticCode::ContractOnVirtualFunction,
                    *name_range,
                    name.clone(),
                    "a virtual function cannot have a contract specifier (C++26)",
                );
                continue;
            }

            if !modifiers.contains(crate::DeclModifiers::DEFAULTED) {
                continue;
            }

            // **Is this the first declaration?** Only then is the program ill-formed. An out-of-class
            // `Widget::Widget() = default;` follows an in-class declaration, and a contract on *it* is legal.
            let first = declarations
                .iter()
                .filter(|(declared, _, _, declared_scope)| {
                    declared == name && declared_scope == scope
                })
                .min_by_key(|(_, _, range, _)| range.start_offset);
            let is_first = first.is_some_and(|(_, _, range, _)| {
                range.start_offset == name_range.start_offset
            });
            if is_first {
                context.add(
                    DiagnosticCode::ContractOnDefaultedFunction,
                    *name_range,
                    name.clone(),
                    "a function defaulted on its first declaration cannot have a contract specifier (C++26)",
                );
            }
        }
    }
}
