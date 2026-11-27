//! **`bitfield-width`**: a bitfield wider than the type it is declared with.
//!
//! ```c
//! struct flags {
//!     unsigned int wide : 40;      // an `unsigned int` is 32 bits: the program does not build
//!     int fine : 3;
//! };
//! ```
//!
//! C11 6.7.2.1p4: "The expression that specifies the width of a bit-field shall ... not exceed the width of the
//! type." Every compiler refuses it, and it was silent here because the parser has no bitfield node: a field written
//! `T name : n` is a declaration whose `InitDeclarator` holds an **`Initializer` that starts with a colon**, which is
//! the shape this rule reads — the parser's own structure, not a text scan.
//!
//! # What is judged, and what is left alone
//!
//! ```text
//!   judged      a width written as a decimal literal, and a type this layer can size
//!   not judged  a type it cannot size      a struct, a typedef, an enum — the size is the target's, not this layer's
//!   not judged  a width that is not a literal  `: N`, `: BITS` — a constant expression this rule does not evaluate
//!   not judged  a width of zero            a zero-width bitfield is legal (it pads to the next unit)
//! ```
//!
//! The sizes are [`crate::sema::layout::a_builtin_in_memory`]'s, which is the same table the member layout uses: the
//! x64 ABI, so an `int` is 32 bits and a `long` is 32 bits on Windows and 64 on Linux — measured, not guessed at by
//! the C standard's minimums.

use cpp_parser::{CppAstNode, CppTokenKind};

use super::{Check, CheckContext, DiagnosticCode};

/// **`bitfield-width`** — see the module documentation.
pub struct BitfieldWidth;

impl Check for BitfieldWidth {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::BitfieldWidth];

    fn check(context: &mut CheckContext<'_>) {
        for initializer in context
            .own::<cpp_parser::CppInitializer>()
            .collect::<Vec<_>>()
        {
            // **The colon is what makes this a bitfield.** A field's `Initializer` also holds `= expr` and `{…}`,
            // and neither is this question.
            let starts_with_a_colon = initializer
                .syntax()
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| {
                    !matches!(
                        CppTokenKind::from(token.kind()),
                        CppTokenKind::Whitespace
                            | CppTokenKind::Newline
                            | CppTokenKind::LineComment
                            | CppTokenKind::BlockComment
                    )
                })
                .is_some_and(|token| CppTokenKind::from(token.kind()) == CppTokenKind::Colon);
            if !starts_with_a_colon {
                continue;
            }

            // **The width, as the literal the file wrote.** A constant expression is not evaluated here: the rule
            // reports only what it can see, and `: WIDTH` is a name this layer would have to fold.
            let Some(width) = initializer
                .syntax()
                .children()
                .find_map(cpp_parser::CppExpr::cast)
                .map(|expression| expression.syntax().clone())
                .and_then(|expression| context.node_text(&expression).trim().parse::<usize>().ok())
            else {
                continue;
            };
            if width == 0 {
                continue;
            }

            // **The type the field is declared with** — the `DeclSpecifierSeq` beside the declarator. A bitfield is a
            // member, so the declaration is a member of a class body and its specifiers are the first child.
            let Some(declared) = the_type_of_the_field(&initializer) else {
                continue;
            };
            let Some((size, _)) = crate::sema::layout::a_builtin_in_memory(&declared) else {
                continue;
            };
            let bits = size * 8;
            if width <= bits {
                continue;
            }

            context.add(
                DiagnosticCode::BitfieldWidth,
                cpp_parser::source_range(initializer.syntax().text_range()),
                declared.clone(),
                t!(
                    "`%{declared}` is %{bits} bits, and this bitfield declares %{width}",
                    declared = declared,
                    bits = bits,
                    width = width
                ),
            );
        }
    }
}

/// **The type spelling of the field a bitfield initialiser belongs to** — the declaration's own specifiers.
///
/// `unsigned int wide : 40` parses as `Declaration(DeclSpecifierSeq(unsigned int), InitDeclarator(name, Initializer(:
/// 40)))`, and `CppDeclaration::get_decl_specifiers` is the typed question for the first of those. Empty when the
/// shape is not that, which is the direction a rule about a *spelling* has to fail in.
fn the_type_of_the_field(initializer: &cpp_parser::CppInitializer) -> Option<String> {
    let declaration = cpp_parser::CppDeclaration::cast(initializer.syntax().parent()?.parent()?)?;
    let specifiers = declaration.get_decl_specifiers()?;
    let written = cpp_parser::subtree_text_spaced(specifiers.syntax());
    let written = written.trim().to_string();
    (!written.is_empty()).then_some(written)
}
