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
            // **A constructor or a destructor promises no return**, and the question is its name's.
            //
            // This used to be implied by "the declaration has no specifier sequence", which is true of the unadorned
            // shapes and false of every decorated one: `explicit A(int x) : x_(x) {}`, `virtual ~A() {}` and
            // `constexpr C(int x) : x_(x) {}` all write specifiers (the parser keeps `explicit`/`virtual`/`constexpr`
            // there, and the first and third keep the constructor's own name too). Measured on a fixture: all three
            // were reported as *"declared to return `` and its body has no `return`"*.
            if declaration.promises_no_return() {
                continue;
            }
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
        // **A `switch` completes when it can be left through its end**, which is provable for the shape a reader
        // writes most: a `default` and not one `break`, so fallthrough carries every path down to the last statement.
        // `try` stays assumed, because an exception may be caught below and that is not readable here.
        CppStat::SwitchStat(switch) => switch_completes(switch),
        CppStat::TryStat(_) => true,
        _ => true,
    }
}

/// **Can a `switch` run to its end?** — the question `LuaParser::BlockFollow` needs, where every label returns.
///
/// The rule proves only the completion that cannot be doubted:
///
/// ```text
///   no `default:`            a value that matches no label skips the whole body — it may complete
///   a `break` anywhere       the break lands after the switch, which is exactly the completion being asked about
///   a `goto` anywhere        the one exit the statement tree does not model
///   last statement completes with none of those, every path that enters a label runs on (fallthrough) to the last
///                            statement, so its end is the switch's end
/// ```
///
/// Measured on `EmmyLuaCodeStyle`'s `LuaParser::BlockFollow`: four `return`s, a `default`, and the rule called the
/// body one that "has no `return`".
fn switch_completes(switch: &cpp_parser::CppSwitchStat) -> bool {
    // **The switch's own children are its body**: the condition, then the labels and statements interleaved
    // (`ParenExpr, CaseStat, ReturnStat, CaseStat, ReturnStat, DefaultStat, ReturnStat`) — this grammar does not put
    // them in a compound block, and looking for one made the whole analysis unreachable.
    let children: Vec<CppStat> = switch
        .syntax()
        .children()
        .filter_map(CppStat::cast)
        .collect();
    // A `default` is what makes "every value takes some label" true.
    let has_default = children
        .iter()
        .any(|statement| matches!(statement, CppStat::DefaultStat(_)));
    if !has_default {
        return true;
    }
    let statements: Vec<&CppStat> = children
        .iter()
        .filter(|statement| !matches!(statement, CppStat::CaseStat(_) | CppStat::DefaultStat(_)))
        .collect();
    // A `break` at any depth of the switch's own body leaves it at its end; a nested loop's or switch's does not.
    if statements.iter().any(|statement| can_be_left(statement)) {
        return true;
    }
    if a_goto_could_leave(switch.syntax()) {
        return true;
    }
    // The last statement decides: with no `break`, every entered label falls through to it.
    let Some(last) = statements.last() else {
        return true;
    };
    completes(last)
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
