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

use super::{Check, CheckContext, DiagnosticCode};
use cpp_parser::CppAstNode;

/// **The range of the name a definition declares**, or `None` when it cannot be located.
///
/// One line over [`super::name_token_of`], because *where* the name is and *what* it is are one question asked of one
/// anchor — a function's name is written immediately before its parameter list's `(`. This function used to answer it
/// a second way, and both ways were wrong before the shared one existed: see that function's note.
///
/// A `None` here is not a lost finding: the caller keeps the whole definition as the range, which is worse to read
/// than a name and far better than nothing.
fn range_of_the_name(function: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::SourceRange> {
    Some(cpp_parser::source_range(
        super::name_token_of(function)?.text_range(),
    ))
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
            // **A spelling that carries a template header is not a return type.** Measured on EmmyLuaCodeStyle: a member
            // function template's fact carried `template<ServiceClass Service, typename... ARGS> void` — the template
            // header and the return type in one string — and the exact comparison below read that as "returns
            // something", which is the false positive this check cannot afford. A spelling with `template` in it is not
            // a type, and a type is what this rule judges.
            if declared.contains("template") {
                continue;
            }

            // **`void` with a suffix** — an attribute, a macro — is still `void`: the exact comparison below would read
            // `void [[nodiscard]]` as a return type and report a body that returns nothing.
            if declared.trim_end().eq("void") || declared.trim_end().ends_with(" void") {
                continue;
            }

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
            // **A constructor is not asked this question**, and the shared reader is the one answer to "is this name the
            // enclosing class's" — the local copy this replaced read the class's name as the last identifier before its body,
            // which the typed layer answers directly (`CppClassDef::get_name_text`).
            if super::name_declared_by(&function)
                .is_some_and(|name| super::declares_the_enclosing_class_named(&function, &name))
            {
                continue;
            }

            // **A coroutine decides what it returns with its promise, not with a `return`.**
            //
            // A body that writes `co_return`, `co_yield` or `co_await` is a coroutine body, and the standard puts
            // the value contract on `promise_type` — `return_void()`, `return_value(v)`, `yield_value(v)` — so a
            // `generator<int>` coroutine that only ever yields is **complete**: falling off its end calls
            // `return_void()`. Reporting it was measured on `test_cpp/coroutine.cpp`: `generator<int> countdown(int
            // from) { for (…;) { co_yield i; } }` answered "this function is declared to return `generator<int>` and
            // its body has no `return`", against a file that compiles and runs.
            //
            // Asked of the **tokens**, not of the statement kinds: `co_yield` is parsed as a throw statement and
            // `co_return` as a return statement, so the tree cannot tell a coroutine from an ordinary function
            // without this spelling test — and `co_await` alone is enough to know the body is one.
            let is_a_coroutine = body.descendants_with_tokens().any(|element| {
                element.into_token().is_some_and(|token| {
                    matches!(
                        cpp_parser::CppTokenKind::from(token.kind()),
                        cpp_parser::CppTokenKind::CoAwaitKeyword
                            | cpp_parser::CppTokenKind::CoYieldKeyword
                            | cpp_parser::CppTokenKind::CoReturnKeyword
                    )
                })
            });
            if is_a_coroutine {
                continue;
            }

            // **A `return` anywhere is enough.** See the module documentation: "not every path returns" is a
            // control-flow question and this layer answers from a tree.
            if !super::the_returns_in(&function).is_empty() {
                continue;
            }

            // **A body whose last statement cannot fall through.** The statement is the node, not a substring of the body:
            // the text version asked whether `throw ` appeared **anywhere** (`int f() { if (x) throw y; }` was silent) and
            // whether a nested `while (true)` ended the text. A wrong "cannot fall through" is a *missed* finding, which is
            // the direction this rule accepts — a wrong "can" is an invented one, which it does not.
            if cpp_parser::CppCompoundStat::cast(body.clone())
                .is_some_and(|body| cannot_fall_through(&body))
            {
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
            let at = range_of_the_name(&function)
                .unwrap_or_else(|| cpp_parser::source_range(function.text_range()));

            context.add(
                DiagnosticCode::MissingReturn,
                at,
                name,
                t!(
                    "this function is declared to return `%{declared}` and its body has no `return`",
                    declared = declared
                ),
            );
        }
    }
}

/// **Can the body's last statement not fall through?** — a `throw`, a `while (true)`, a `for (;;)`, or a braced block whose own
/// last statement says so.
///
/// The loop spellings are read from the statement's own text because `CppForStat` does not expose its condition yet; a `break`
/// inside the loop is not considered, which is the conservative direction (a missed finding, never an invented one).
fn cannot_fall_through(body: &cpp_parser::CppCompoundStat) -> bool {
    use cpp_parser::{CppAstNode, CppStat};

    let Some(last) = body.get_stats().last() else {
        return false;
    };
    match &last {
        CppStat::ThrowStat(_) => return true,
        CppStat::WhileStat(while_statement) => {
            let condition = while_statement
                .get_condition()
                .map(|condition| condition.syntax().clone());
            return is_the_literal_true(condition.as_ref());
        }
        CppStat::CompoundStat(block) => return cannot_fall_through(block),
        _ => {}
    }
    let spelled = last.syntax().text().to_string();
    let spelled = spelled.trim_start();
    spelled.starts_with("for (;;)") || spelled.starts_with("for(;;)")
}

fn is_the_literal_true(condition: Option<&cpp_parser::CppSyntaxNode>) -> bool {
    use cpp_parser::{CppAstNode, CppExpr};
    let Some(condition) = condition else {
        return false;
    };
    match CppExpr::cast(condition.clone()) {
        Some(CppExpr::ParenExpr(paren)) => matches!(
            paren.get_expr(),
            Some(CppExpr::LiteralExpr(literal))
                if literal.get_literal_text().is_some_and(|text| text.trim() == "true")
        ),
        _ => false,
    }
}
