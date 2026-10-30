//! **`cstyle-cast`** — a cast written in the C style, `(Widget*)p`, where C++ has four named casts to say which conversion
//! is meant.
//!
//! # Why this reads the tree
//!
//! A C-style cast is a `CastExpr` with a **`TypeId`** child: the `(Widget*)` part names the type being cast to. The named
//! casts are different shapes entirely (`static_cast<…>(…)` carries a template-argument list), and a functional cast
//! (`int(x)`) is a call. So the question is structural, and it cannot produce a finding that is not a cast.
//!
//! Before this, the rule paired `(` with `)` in the file's raw text. That needed three separate defences against things
//! that look like a cast and are not — a preprocessor condition, a parameter list, a template-argument list — and each of
//! those defences was added because the census found the false positive it fixes (813, then 376, then 285 findings on one
//! real project). The tree has none of those shapes: a cast the preprocessor skipped is not in the tree, and a `(` in a
//! parameter list is a `ParameterList` node.

use cpp_parser::{CppAstNode, CppSyntaxKind};

use super::{Check, CheckContext, DiagnosticCode};

/// **`cstyle-cast`** — see the module documentation.
pub struct CStyleCast;

impl Check for CStyleCast {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::CStyleCast];

    fn check(context: &mut CheckContext<'_>) {
        // **Not a rule in C.** C has no `static_cast`, no `const_cast` and no `reinterpret_cast` — `(Widget*)p` is how a C
        // program writes that conversion, and a rule that called it a mistake there would be wrong about the language.
        if context.is_c() {
            return;
        }

        let Some(root) = context.root() else {
            return;
        };
        for node in root.syntax().descendants() {
            if CppSyntaxKind::from(node.kind()) != CppSyntaxKind::CastExpr {
                continue;
            }
            // **The `TypeId` is what makes it the C style.** A named cast carries a template-argument list instead, and a
            // functional cast is a call — neither has a type-id child.
            // **A C-style cast starts with its `(`.** `static_cast<int>(n)` is a `CastExpr` too, and its type-id child looks
            // the same — the first token is what tells them apart, and it is a token of this node rather than a search of the
            // file's text.
            let opens_with_a_parenthesis = node
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| !token.text().trim().is_empty())
                .is_some_and(|token| token.text() == "(");
            if !opens_with_a_parenthesis {
                continue;
            }
            let Some(type_id) = node
                .children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TypeId)
            else {
                continue;
            };
            let written = type_id.text().to_string();
            let written = written.trim();
            if written.is_empty() {
                continue;
            }
            // **A cast to `void` is not a conversion**, and the message would name a spelling that does not exist:
            // `(void)expression` is how C++ says "evaluate this and discard the result", and there is no
            // `static_cast<void>(…)` in the language this rule was borrowed from. Every C++ codebase writes it,
            // including the macro that made this finding on `preprocessor.cpp` of the C++ corpus.
            if written == "void" {
                continue;
            }
            context.add(
                DiagnosticCode::CStyleCast,
                cpp_parser::source_range(node.text_range()),
                t!("(%{written})", written = written),
                t!(
                    "this is a C-style cast to `%{written}` — the four named casts say which conversion is meant, and \
                     `static_cast<%{written}>(…)` is almost always the one",
                    written = written
                ),
            );
        }
    }
}
