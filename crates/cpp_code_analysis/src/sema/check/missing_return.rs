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
//!   reported      a body that **can reach its closing brace** and a written return type that is not `void`
//!   not reported  `void f() { }`             nothing is expected
//!   not reported  `auto f() { … }`           a deduced return type, which this does not read
//!   not reported  a body **no path leaves normally** — every path returns, throws, or loops forever
//!   not reported  a **declaration**          `int f();` is a promise, not a body
//! ```
//!
//! # Why "can reach the closing brace" rather than "no `return` anywhere"
//!
//! Because "no `return` anywhere" is silent for the shape a reader actually writes by accident:
//! `int other(bool x) { if (x) return 1; }` has a `return`, and still falls off its end. The old rule also decided a few
//! shapes by searching the body's **text** — `while (true)`, `throw ` anywhere in it, `for (;;)` — which is wrong in both
//! directions: `int f(bool x) { if (x) throw E(); }` has the word and still falls through, and `for (;;)` written with no
//! space was a different answer.
//!
//! So the question is the control-flow one, asked of the statement tree (see [`reaches_the_end`]). It answers "no path
//! reaches the end" only when it can **prove** it, because the two mistakes are not equal: a wrong "reaches the end" is a
//! finding about code that is correct, and a wrong "does not" is a finding nobody sees. `switch` and `try` are assumed to
//! be able to complete, for the same reason.
//!
//! # Only this file
//!
//! A function's body is in the file being checked, so this is a question about one file by construction.

use cpp_parser::{CppAstNode, CppStat};

use super::{Check, CheckContext, DiagnosticCode};

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
            let Some(declaration) = cpp_parser::CppDeclaration::cast(function.clone()) else {
                continue;
            };
            let Some(body) = declaration.get_body() else {
                continue;
            };
            // **The written return type, as a node.** A declaration with no specifier sequence of its **own** has no
            // written return type: a constructor, a destructor, and an out-of-class definition whose head the grammar could
            // not read all answer `None` here. That is the answer the old rule got wrong by falling through to the
            // parameter list and reading the first parameter's specifier.
            let Some(specifiers) = declaration.get_decl_specifiers() else {
                continue;
            };
            // **`void` and the deduced types** — asked of the specifier sequence, not of a spelling: `void [[nodiscard]]`,
            // `auto`, `auto&` and `decltype(auto)` are all "nothing is promised here".
            if specifiers.is_void() || specifiers.is_deduced_return_type() {
                continue;
            }
            // **A declaration whose declarator names nothing is not a function this rule can judge.**
            //
            // An out-of-class constructor or destructor reaches this point with its qualified name read as the
            // *type* and no declarator name at all — `LSPHandle::LSPHandle(…)` is `DeclSpecifierSeq "LSPHandle::LSPHandle"`
            // plus a declarator that is only `(…)`, the shape `sema::scopes` reads as the declaration's own name
            // (see its `qualified_specifier_name`). There is no name to report and no promise to check, so the rule
            // must not speak. Measured in a user's file:
            //
            // ```text
            // LSPHandle::LSPHandle(LanguageServer *server) : _server(server) { Initialize(); }
            //   this function is declared to return `` and its body has no `return`     ← an empty spelling
            // ```
            //
            // The empty spelling is the same mistake as the finding. The deeper fix is the grammar's (an out-of-class
            // declaration's qualified name belongs in the declarator, not in the specifier sequence); it is recorded
            // in `docs/buglist.md` §29 with the completion path that has to change with it.
            // The test is the **spelling**: `S::S` is a constructor and `S::~S` a destructor, and neither promises a
            // return. The specifier sequence of such a declaration is literally that qualified name — the shape
            // `sema::scopes` reads the declaration's own name from (see its `qualified_specifier_name`) — so the two
            // segments are compared: `S::f` is a definition of a function that does promise one, `S::S` is not.
            let written: String = specifiers
                .syntax()
                .text()
                .to_string()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if let Some((qualifier, last)) = written.rsplit_once("::") {
                let class = qualifier.rsplit("::").next().unwrap_or(qualifier);
                if last == class || last.starts_with('~') {
                    continue;
                }
            }

            // **A coroutine decides what it returns with its promise, not with a `return`.**
            //
            // A body that writes `co_return`, `co_yield` or `co_await` is a coroutine body, and the standard puts the value
            // contract on `promise_type` — `return_void()`, `return_value(v)`, `yield_value(v)` — so a `generator<int>`
            // coroutine that only ever yields is **complete**: falling off its end calls `return_void()`. Reporting it was
            // measured on `test_cpp/coroutine.cpp`: `generator<int> countdown(int from) { for (…;) { co_yield i; } }`
            // answered "this function is declared to return `generator<int>` and its body has no `return`", against a file
            // that compiles and runs.
            //
            // Asked of the **tokens**, not of the statement kinds: `co_yield` is parsed as a throw statement and `co_return`
            // as a return statement, so the tree cannot tell a coroutine from an ordinary function without this spelling
            // test — and `co_await` alone is enough to know the body is one.
            let is_a_coroutine = body.syntax().descendants_with_tokens().any(|element| {
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

            // **The control-flow question**, and the whole of the rule now: does any path reach the closing brace?
            if !reaches_the_end(&body) {
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
            // The sentence names the type as the reader wrote it, which is the one place a spelling is wanted.
            let declared = super::return_type_written_by(&function).unwrap_or_default();

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

/// **Can any path reach the end of this body?**
///
/// A path reaches the end when it neither returns, throws, nor is trapped in a loop that cannot exit — and the answer is
/// `true` unless **every** path is one of those.
fn reaches_the_end(body: &cpp_parser::CppCompoundStat) -> bool {
    statements_reach_the_end(body.get_stats())
}

fn statements_reach_the_end(statements: impl IntoIterator<Item = CppStat>) -> bool {
    for statement in statements {
        if !completes(&statement) {
            // Nothing after a statement that cannot complete is reachable, so this sequence cannot either.
            return false;
        }
    }
    true
}

/// Can this statement run to its end, so that the statement after it runs?
fn completes(statement: &CppStat) -> bool {
    match statement {
        // A block is its statements.
        CppStat::CompoundStat(block) => statements_reach_the_end(block.get_stats()),
        // The two statements after which control does not continue in this function. (`co_return` parses as a `return`,
        // and a coroutine body never gets here — see the check above.)
        CppStat::ReturnStat(_) | CppStat::ThrowStat(_) => false,
        // **An `if` completes unless both of its branches are exhaustive.** A literal condition decides which branch
        // exists, and `if constexpr` with one is the same question one language level up.
        CppStat::IfStat(branch) => {
            let condition = condition_is_true(branch.get_condition());
            if let Some(taken) = condition {
                return if taken {
                    branch
                        .get_then_branch()
                        .is_none_or(|branch| completes(&branch))
                } else {
                    branch.get_else_branch().is_none_or(|other| {
                        other.get_stat().is_none_or(|branch| completes(&branch))
                    })
                };
            }
            let then_completes = branch
                .get_then_branch()
                .is_none_or(|branch| completes(&branch));
            let else_completes = branch
                .get_else_branch()
                .is_none_or(|other| other.get_stat().is_none_or(|branch| completes(&branch)));
            then_completes || else_completes
        }
        // **A loop completes when it can be left.** An infinite loop with no `break` (and no `goto` that could leave it) is
        // the third way a body ends without executing its last statement.
        CppStat::WhileStat(loop_) => {
            let endless = condition_is_true(loop_.get_condition()) == Some(true);
            if !endless {
                return true;
            }
            loop_
                .get_body()
                .is_none_or(|body| can_be_left(&body) || a_goto_could_leave(body.syntax()))
        }
        CppStat::DoWhileStat(loop_) => {
            let body = loop_.get_body();
            if body.as_ref().is_some_and(|body| !completes(body)) {
                return false;
            }
            let endless = condition_is_true(loop_.get_condition()) == Some(true);
            !endless
                || body.is_some_and(|body| can_be_left(&body) || a_goto_could_leave(body.syntax()))
        }
        // **`for (;;)` is an endless loop; a `for` with a condition can exit.** The head is asked as a node question, because
        // the two spellings differ by what is written between the `;` and not by a word.
        CppStat::ForStat(loop_) => {
            if loop_.has_a_condition() {
                return true;
            }
            loop_
                .get_body()
                .is_none_or(|body| can_be_left(&body) || a_goto_could_leave(body.syntax()))
        }
        // **`switch` and `try` are assumed to be able to complete**, and the assumption is the direction the rule needs: a
        // case with no `break` falls through, an exception may be caught below, and a wrong "cannot complete" here would be
        // an invented finding.
        CppStat::SwitchStat(_) | CppStat::TryStat(_) => true,
        _ => true,
    }
}

/// **Can this loop body be left from inside it?** — a `break`, through any block or `if` of the loop's own, but not through
/// a nested loop's or a nested `switch`'s, whose breaks belong to them.
fn can_be_left(statement: &CppStat) -> bool {
    match statement {
        CppStat::BreakStat(_) => true,
        CppStat::CompoundStat(block) => {
            block.get_stats().into_iter().any(|stat| can_be_left(&stat))
        }
        CppStat::IfStat(branch) => {
            branch
                .get_then_branch()
                .is_some_and(|branch| can_be_left(&branch))
                || branch
                    .get_else_branch()
                    .and_then(|other| other.get_stat())
                    .is_some_and(|branch| can_be_left(&branch))
        }
        CppStat::ElseStat(other) => other.get_stat().is_some_and(|branch| can_be_left(&branch)),
        _ => false,
    }
}

/// **Is the condition a literal whose truth is known?** — `true`/`false`, and the integer spellings C writes them with. `None`
/// when the condition says nothing (`x`, `i < n`), which is the answer that keeps the caller conservative.
fn condition_is_true(condition: Option<cpp_parser::CppParenExpr>) -> Option<bool> {
    let condition = condition?;
    let expression = condition.get_expr()?;
    let cpp_parser::CppExpr::LiteralExpr(literal) = expression else {
        return None;
    };
    match literal.get_literal_text()?.trim() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// **Could a `goto` leave this loop?** — the one exit the statement tree does not model. Asked of the tokens, because a label
/// is not a construct any statement owns.
fn a_goto_could_leave(body: &cpp_parser::CppSyntaxNode) -> bool {
    body.descendants_with_tokens().any(|element| {
        element.into_token().is_some_and(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::GotoKeyword
        })
    })
}
