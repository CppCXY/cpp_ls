//! **Ascend C's AI Core restrictions** — constructs the device compiler does not accept, reported only inside a
//! kernel: a function whose declaration carries `__aicore__` or `__global__`.
//!
//! ```cpp
//! __global__ __aicore__ void bad() {
//!     auto *p = new int[16];          // ascend-dynamic-allocation
//!     try { work(); } catch (...) { }  // ascend-exception
//!     std::vector<int> values;         // ascend-std-type
//! }
//! ```
//!
//! **The tree, not the text.** The kernel is a `Declaration` whose `DeclSpecifierSeq` carries the qualifier, and each
//! finding is a node of its own kind (`NewExpr`, `TryStat`, `CatchStat`, `ThrowExpr`, `DeclSpecifierSeq`). The check
//! walks the CST once per kernel and asks the node kinds; a substring search would match comments, strings and an
//! entirely different host function.

use cpp_parser::{CppAstNode, CppDeclaration, CppSyntaxKind, CppTokenKind};

use super::{Check, CheckContext, DiagnosticCode};

pub struct AscendKernel;

impl Check for AscendKernel {
    const CODES: &'static [DiagnosticCode] = &[
        DiagnosticCode::AscendDynamicAllocation,
        DiagnosticCode::AscendException,
        DiagnosticCode::AscendStdType,
    ];

    fn check(context: &mut CheckContext<'_>) {
        for declaration in context.own::<CppDeclaration>().collect::<Vec<_>>() {
            let Some(specifiers) = declaration.get_decl_specifiers() else {
                continue;
            };
            let Some(body) = declaration.get_body() else {
                continue;
            };
            if !a_kernel_declaration(&specifiers) {
                continue;
            }

            for node in body.syntax().descendants() {
                let kind = CppSyntaxKind::from(node.kind());
                match kind {
                    CppSyntaxKind::NewExpr | CppSyntaxKind::DeleteExpr => {
                        let range = cpp_parser::source_range(node.text_range());
                        let text = context
                            .source
                            .get(range.start_offset..range.end_offset())
                            .unwrap_or_default();
                        context.add(
                            DiagnosticCode::AscendDynamicAllocation,
                            range,
                            text.to_string(),
                            "Ascend C kernel code cannot allocate or free heap memory; use a `TPipe` buffer or a \
                             tensor allocated from on-chip memory.",
                        );
                    }
                    CppSyntaxKind::TryStat
                    | CppSyntaxKind::CatchStat
                    | CppSyntaxKind::ThrowStat
                    | CppSyntaxKind::ThrowExpr => {
                        let range = cpp_parser::source_range(node.text_range());
                        let text = context
                            .source
                            .get(range.start_offset..range.end_offset())
                            .unwrap_or_default();
                        context.add(
                            DiagnosticCode::AscendException,
                            range,
                            text.to_string(),
                            "C++ exceptions are not available on the Ascend AI Core; return an error code or handle \
                             the condition explicitly.",
                        );
                    }
                    CppSyntaxKind::DeclSpecifierSeq => {
                        report_a_std_type(context, &node);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// **A `std::` name in a declaration's specifier sequence.** The tokens are the CST's, and the spelling is exact:
/// `std`, `:`, `:`, and the type name. Only the type list below is a device restriction; the rest may be a host
/// declaration in the same translation unit, which is not this kernel's body.
fn report_a_std_type(context: &mut CheckContext<'_>, specifiers: &cpp_parser::CppSyntaxNode) {
    let tokens: Vec<_> = specifiers
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        if token.text() != "std" {
            continue;
        }
        // The lexer may spell `::` as one token or as two; both are the same qualification.
        let name_index = match tokens.get(index + 1).map(|next| next.text()) {
            Some("::") => index + 2,
            Some(":") if tokens.get(index + 2).is_some_and(|next| next.text() == ":") => index + 3,
            _ => continue,
        };
        let Some(name) = tokens.get(name_index).map(|next| next.text()) else {
            continue;
        };
        if !is_a_device_hostile_type(name) {
            continue;
        }
        let start = usize::from(token.text_range().start());
        let end = usize::from(tokens[name_index].text_range().end());
        context.add(
            DiagnosticCode::AscendStdType,
            cpp_parser::SourceRange::new(start, end.saturating_sub(start)),
            format!("std::{name}"),
            format!(
                "`std::{name}` is a host-side standard-library type; Ascend C kernels use `GlobalTensor`/`LocalTensor` \
                 and the AICore intrinsics instead."
            ),
        );
    }
}

/// **Is this declaration the AI Core kernel?** The qualifiers are in the declaration's specifier sequence, and the
/// tokens are the evidence; a host function beside the kernel has no such sequence.
fn a_kernel_declaration(specifiers: &cpp_parser::CppDeclSpecifierSeq) -> bool {
    has_an_aicore_qualifier(specifiers.syntax())
}

fn has_an_aicore_qualifier(node: &cpp_parser::CppSyntaxNode) -> bool {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            CppTokenKind::from(token.kind()) == CppTokenKind::Identifier
                && matches!(token.text(), "__aicore__" | "__global__")
        })
}

/// The standard-library types the device compiler does not provide. A conservative list on purpose: a false positive
/// in a kernel is a line the reader cannot act on, and a type not listed here remains visible as an ordinary host
/// declaration.
fn is_a_device_hostile_type(name: &str) -> bool {
    matches!(
        name,
        "vector"
            | "string"
            | "map"
            | "unordered_map"
            | "set"
            | "unordered_set"
            | "list"
            | "deque"
            | "array"
            | "tuple"
            | "function"
            | "shared_ptr"
            | "unique_ptr"
            | "optional"
            | "variant"
    )
}
