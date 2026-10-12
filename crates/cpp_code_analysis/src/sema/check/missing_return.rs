//! **`missing-return`**: a function declared to return something, whose body never returns.
//!
//! ```cpp
//! int count() { }                    // nothing is returned, and the type says something must be
//! int other() { if (x) return 1; }   // …and here only one path does
//! ```
//!
//! # What is reported, and the one shape that is not
//!
//! ```text
//!   reported      a body with **no `return` statement at all** and a written return type that is not `void`
//!   not reported  `void f() { }`             nothing is expected
//!   not reported  `auto f() { … }`           a deduced return type, which this does not read
//!   not reported  a body whose last statement cannot fall through — a `throw`, a call to something that does not
//!                 return, an infinite loop. The last is not checked *syntactically* here: a body ending in
//!                 `while (true)` is recognised by its text, and anything more clever belongs in a control-flow
//!                 analysis rather than in a check that walks a tree.
//!   not reported  a **declaration**            `int f();` is a promise, not a body
//! ```
//!
//! # Why "no `return` at all" rather than "not every path returns"
//!
//! Because the second question is a control-flow question and this layer answers from a tree. A walk that tried to
//! decide whether every path returns would have to model `if`/`else`, `switch`, `goto`, exceptions and loops — and a
//! version of that which is *nearly* right reports functions that are correct, which is the failure this layer is
//! written against. The shape that is left is small, unambiguous, and the one a reader actually writes by accident:
//! a body that was never finished.
//!
//! # Only this file
//!
//! A function's body is in the file being checked, so this is a question about one file by construction.

use cpp_parser::CppSyntaxKind;

use super::{Check, CheckContext, DiagnosticCode};

/// **Does this definition declare the class it is written inside** — a constructor, or a destructor?
///
/// The question a definition's *name* answers, which is why it is asked of the name and not of a return type: a
/// constructor writes `Name(…)` and has no return type to read, so any answer to "what does it return" is a
/// fabrication. Measured on `struct Name { Name(const std::string& f) : firstName(f) {} };` — the declaration has no
/// `DeclSpecifierSeq` of its own (its ancestry is `Declaration > ClassBody > StructDef`), and the helper that reads a
/// return type fell through to the parameter list and answered `Some("Name")`.
///
/// # Why the enclosing class's name is read rather than its scope
///
/// Because a check is handed a tree and not a scope table ([`CheckContext`]'s note), and because the name is the
/// evidence the language itself uses: `Name` inside `class Name` is a constructor and `Name` anywhere else is a
/// type. Both spellings of the destructor are matched, since `~Name` and `Name::~Name` are the same declaration
/// written two ways.
fn declares_the_enclosing_class(function: &cpp_parser::CppSyntaxNode) -> bool {
    use cpp_parser::CppSyntaxKind;

    let Some(name) = super::name_declared_by(function) else {
        return false;
    };

    for ancestor in function.ancestors().skip(1) {
        match CppSyntaxKind::from(ancestor.kind()) {
            CppSyntaxKind::StructDef | CppSyntaxKind::ClassDef | CppSyntaxKind::UnionDef => {
                // The class's own name is the last identifier before its body — the same "last one written" rule
                // [`super::spelling_of`] uses for a declarator.
                let declared = ancestor
                    .children()
                    .find(|child| {
                        matches!(
                            CppSyntaxKind::from(child.kind()),
                            CppSyntaxKind::ClassBody 
                        )
                    })
                    .map(|body| {
                        ancestor
                            .descendants_with_tokens()
                            .filter_map(|element| element.into_token())
                            .take_while(|token| token.text_range().end() <= body.text_range().start())
                            .filter(|token| token.kind() == cpp_parser::CppTokenKind::Identifier.into())
                            .last()
                            .map(|token| token.text().to_string())
                    })
                    .flatten();
                return declared.as_deref() == Some(name.trim_start_matches('~'));
            }
            // A function written inside a function body is a local class or a lambda's, and neither is this
            // question.
            CppSyntaxKind::CompoundStat => return false,
            _ => {}
        }
    }
    false
}

/// **The range of the name a definition declares**, when it can be located.
///
/// The **last non-trivia token before the parameter list's `(`** — where a function's name is written in every
/// spelling this check sees: `int f()`, `void C::f()`, `Name::~Name()`.
///
/// # Why not the helpers that answer *what* the name is
///
/// `super::name_declared_by` and `super::spelling_of` read the declaration's *head* as the children before the
/// parameter list. That is the right question for a **declaration** and it is empty for a **definition**, because
/// there the parameter list sits inside the declarator: measured on `int unfinished() { }`,
/// `name_declared_by -> None` while `return_type_written_by -> Some("int")`.
///
/// So a name-shaped range taken from them is `None` for every function body — and the first version of this helper
/// did worse than that: it scanned for the first identifier *spelled like* the (empty) name, matched the return type
/// `int`, and produced a range running from `int` to the end of the file.
fn range_of_the_name(function: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::SourceRange> {
    use cpp_parser::{CppAstNode, CppParamList};

    // **The typed AST, not a kind comparison.** The node kind is `ParameterList`; an earlier version of this helper
    // compared against `CppSyntaxKind::ParamList`, which does not exist as a kind this grammar produces — so the
    // guard answered `None` for every function and the range fell back to the whole definition. `status.md` §5 names
    // this exact trap: *"判断用强类型 AST（`cast`），而不是手写 `CppSyntaxKind` 比较"*. Measured, the node is
    // `ParameterList at 14..17 "() "` and `CppParamList::cast` accepts it.
    let parameters = function
        .descendants()
        .find_map(CppParamList::cast)?;
    let open = usize::from(parameters.syntax().text_range().start());

    function
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| usize::from(token.text_range().end()) <= open)
        .filter(|token| !cpp_parser::is_trivia(cpp_parser::CppTokenKind::from(token.kind())))
        .filter(|token| token.kind() == cpp_parser::CppTokenKind::Identifier.into())
        .last()
        .map(|token| cpp_parser::source_range(token.text_range()))
}

/// **`missing-return`** — see the module documentation.
pub struct MissingReturn;

impl Check for MissingReturn {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::MissingReturn];

    fn check(context: &mut CheckContext<'_>) {
        // **This file's own functions**, typed - see `CheckContext::own_syntax_nodes`, which prunes the inlined
        // headers a rendering splices in. Collected before the loop because a finding needs `&mut context`.
        for function in super::function_definitions(context) {
            let Some(body) = super::body_of(&function) else {
                continue;
            };

            let Some(declared) = super::return_type_written_by(&function) else {
                continue;
            };
            if declared == "void" || declared == "auto" || declared == "decltype(auto)" {
                continue;
            }
            // **A constructor is not asked this question, and the guard above does not answer it.**
            //
            // The note that used to stand here said "a constructor has no return type and a destructor's is not
            // written; both are `None` above" — and that premise was **false**, which is why this check reported
            // `this function is declared to return `Name`` on
            // `Name(const std::string& f, …) : firstName(f) { }`. Measured, the two definitions and what the helper
            // answers for each:
            //
            // ```text
            //   Declaration "Name(const std::string& f)…"   DeclSpecifierSeq children: ["Name"]    -> Some("Name")
            //   Declaration "int value() {}"                DeclSpecifierSeq children: ["int "]     -> Some("int")
            // ```
            //
            // The constructor's declaration has **no** `DeclSpecifierSeq` of its own — its ancestry is
            // `Declaration > ClassBody > StructDef` — so the helper fell through to the *parameter list* and read the
            // first declarator's specifier out of it. Asking a constructor for its return type is a question with no
            // answer, and the honest place to say so is here rather than in the helper: the helper is also asked
            // about members, where a class name would be a real answer.
            if declares_the_enclosing_class(&function) {
                continue;
            }

            // **A `return` anywhere is enough.** See the module documentation: "not every path returns" is a
            // control-flow question and this layer answers from a tree.
            let has_a_return = body
                .descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ReturnStat);
            if has_a_return {
                continue;
            }

            // **A body whose last statement cannot fall through.** Read as text, because "does this end in a
            // `throw` or in a loop that never ends" is a question about the last few tokens rather than about the
            // shape of a tree, and because a wrong "it cannot fall through" here is a *missed* finding rather than
            // an invented one.
            let text = body.text().to_string();
            let tail = text.trim_end_matches(['}', ' ', '\n', '\t', ';']);
            let cannot_fall_through = tail.ends_with("throw")
                || tail.contains("throw ")
                || tail.ends_with("while (true)")
                || tail.ends_with("while(true)")
                || tail.ends_with("for (;;)")
                || tail.ends_with("for(;;)");
            if cannot_fall_through {
                continue;
            }

            // **The range carries the name, so the message does not have to.**
            //
            // It reported the whole `function.text_range()`, and the message spells the name out again — so a
            // twenty-line constructor was underlined top to bottom and the sentence repeated what the squiggle
            // should have pointed at. The name is what the reader has to change, and `name_declared_by` is the same
            // helper the message already uses. A function the helper cannot name keeps the whole range rather than
            // losing the finding: a squiggle that is too wide is a worse answer than none, but a finding that
            // disappears because this layer could not locate one word is worse than both.
            let name = super::name_declared_by(&function).unwrap_or_default();
            let at = range_of_the_name(&function).unwrap_or_else(|| cpp_parser::source_range(function.text_range()));

            context.add(
                DiagnosticCode::MissingReturn,
                at,
                name,
                format!(
                    "this function is declared to return `{declared}` and its body has no `return`"
                ),
            );
        }
    }
}








