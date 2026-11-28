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

use crate::DeclKind;
use crate::Known;
use crate::sema;
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

/// **What the declared return type promises**, as far as *this reading* can tell.
///
/// Three answers rather than a boolean, because "is it void" and "is it not void" are not each other's negation here:
/// a spelling the reading cannot resolve is neither, and a rule that has to be sure before it speaks needs a place to
/// stand when it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Promise {
    /// Nothing is returned — `void`, or a name this reading says is `void` (`typedef void nothing`).
    Nothing,
    /// A value is returned — a scalar, a pointer, a reference, a class — so a body that falls off its end is a finding.
    AValue,
    /// This reading cannot tell: a deduced type, or a name nothing it can see declares.
    NotKnown,
}

/// **What one declaration promises**, read from the specifiers and the declarator, not from a spelling.
///
/// The spelling is what the *message* wants; the question is the tree's, because `void *p()` and `void p()` are the
/// same two keywords and different promises. Measured with a fixture: the keyword test this replaced (`is_void`)
/// treated `void *p() { }` as "nothing is promised" and was **silent about a function that returns nothing at all** —
/// a pointer is a value whatever precedes it.
///
/// The name arm is what keeps the rule honest when the reading has not expanded something. `static l_noret typeerror(…)`
/// is Lua's own spelling: `l_noret` is `void __declspec(noreturn)` (or `void __attribute__((noreturn))`, or plain
/// `void`), and in a reading that does not carry that `#define` the spelling is a name nothing declares — the rule read
/// it as "not void" and reported *"this function is declared to return `l_noret` and its body has no `return`"* on a
/// function whose whole purpose is not to return. A name the reading cannot resolve is not evidence of a value, and a
/// rule that has to be sure does not speak.
fn the_promise_of(context: &CheckContext<'_>, declaration: &cpp_parser::CppDeclaration) -> Promise {
    use cpp_parser::CppAstNode;
    let Some(specifiers) = declaration.get_decl_specifiers() else {
        return Promise::NotKnown;
    };
    // A deduced return type promises nothing this rule can read.
    if specifiers.is_deduced_return_type() {
        return Promise::NotKnown;
    }
    // **A pointer or a reference returns a value whatever precedes it.** Asked of the declarator, which is where the
    // `*` is: `void *p` writes the `void` keyword too, and the keyword alone cannot tell the two apart.
    let pointer_or_reference = declaration
        .get_init_declarator()
        .and_then(|declarator| declarator.child::<cpp_parser::CppDeclarator>())
        .is_some_and(|declarator| declarator.is_a_pointer() || declarator.is_a_reference());
    if pointer_or_reference {
        return Promise::AValue;
    }
    if specifiers.is_void() {
        return Promise::Nothing;
    }
    if specifiers.get_builtin_type().is_some() {
        return Promise::AValue;
    }
    let Some(name) = specifiers.get_type_name() else {
        return Promise::NotKnown;
    };
    the_promise_of_a_name(context, &name.get_qualified_name(), 0)
}

/// **What a name promises** — followed through the index to the declaration it names, and through a `typedef`'s target.
///
/// `typedef void nothing; nothing f() { }` is the shape this exists for: the name is a type, and a type that *is* `void`
/// promises nothing, so "not the keyword `void`" is not the same as "a value". Two answers are read from a target
/// without asking again — `void` itself, and the built-ins (`typedef int myint;`) — and a target that is another name
/// is followed. The walk is bounded because a `typedef` chain can be a cycle (`typedef A B; typedef B A;` is
/// ill-formed but readable) and three hops answer every real spelling.
///
/// A target this cannot classify — an attributéd `void`, a template-id — answers `NotKnown` rather than guessing: the
/// safe direction is silence, and the rule's other arms already report the shapes that are certain.
fn the_promise_of_a_name(context: &CheckContext<'_>, spelling: &str, depth: usize) -> Promise {
    let spelled = spelling.trim();
    if spelled.is_empty() || depth > 3 {
        return Promise::NotKnown;
    }
    let Known::Yes(known) = context.index().definition(spelled, context.path) else {
        return Promise::NotKnown;
    };
    let fact = &known.fact;
    // A class, a struct, an enum — something with a size, which is a value.
    if fact.kind != DeclKind::Type {
        return Promise::AValue;
    }
    match fact.type_of.as_deref().map(str::trim) {
        // A type declared through its own name (`class Widget;`) is an ordinary class.
        None | Some("") => Promise::AValue,
        Some("void") => Promise::Nothing,
        // **A built-in target is a value** — `typedef int myint;` — and a pointer to anything is a value too.
        Some(target)
            if target.contains('*')
                || target.contains('&')
                || sema::layout::a_builtin_in_memory(target).is_some() =>
        {
            Promise::AValue
        }
        Some(target) => the_promise_of_a_name(context, target, depth + 1),
    }
}
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
            // **What the return type promises**, which is the whole of the rule's premise. A pointer decides by its
            // declarator (`void *p` is a value), a keyword by the specifier sequence, and a *name* by what the index
            // says it declares — see [`the_promise_of`], where the three answers and the ones they replaced are
            // recorded.
            if the_promise_of(context, &declaration) != Promise::AValue {
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
            //
            // **One token walk answers both token questions** — this one and `goto`, which the flow walk below needs to
            // know whether an endless loop can be left. It used to be asked per endless loop, over that loop's whole
            // subtree; one pass over the body is the same answer at a fixed price.
            let (is_a_coroutine, has_a_goto) = token_facts(&body);
            if is_a_coroutine {
                continue;
            }

            // **The control-flow question**, and the whole of the rule now: can any path reach the closing brace?
            if !the_body_falls_off_its_end(&body, has_a_goto) {
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

/// **What one statement says about the two things the rule asks of it**, read in a **single traversal**.
///
/// The rule has exactly one question — *can control reach the point after this statement?* — and one piece of
/// context an enclosing statement needs from its parts: *does any path leave it through a `break` that belongs to me?*
/// Every statement answers both, and the composite kinds combine them. That is the whole algorithm: one recursive
/// walk over the statement tree, each node visited once.
///
/// # Why this shape, and not the one it replaced
///
/// The rule used to be a set of predicates that each walked a subtree to answer their own question: `can_be_left`
/// descended into every block and `if` of a loop to look for a `break`, and `a_goto_could_leave` scanned a loop's
/// **whole subtree's tokens** — so a statement nested under a loop was visited once per enclosing loop, and a deeply
/// nested body was quadratic in its own size. The answer was right; the shape made the price depend on nesting rather
/// than on size.
///
/// Two facts stay **per body** rather than per statement, because they are the same everywhere in it: whether the body
/// writes a coroutine keyword (then the whole rule does not apply) and whether it writes a `goto` anywhere (which can
/// leave any loop, so an endless loop containing one cannot be called endless). Both are read in one token pass — see
/// [`token_facts`].
#[derive(Clone, Copy)]
struct Flow {
    /// Can control reach the point right after this statement?
    reaches: bool,
    /// Does some path leave through a `break` that belongs to the loop or `switch` **around** this statement?
    ///
    /// A `break` inside a loop or a `switch` belongs to *that* statement and stops there: it never reaches the
    /// enclosing one, which is why a loop's own flow reports `breaks: false`.
    breaks: bool,
}

impl Flow {
    /// A statement control runs straight past: every expression statement, declaration and label.
    const FALLS_THROUGH: Flow = Flow {
        reaches: true,
        breaks: false,
    };

    /// A statement that cannot be run past — `return`, `throw`, `break`, `continue`.
    const STOPS: Flow = Flow {
        reaches: false,
        breaks: false,
    };
}

/// **The two token facts the flow walk needs**, read in one pass over the body.
///
/// * a **coroutine keyword** (`co_await`, `co_yield`, `co_return`) — parsed as ordinary statements, so the tree cannot
///   tell a coroutine from a function, and a coroutine's value contract is its promise's rather than a `return`'s;
/// * a **`goto`** — the one exit a statement tree cannot model, so an endless loop that contains one may be left.
///
/// Asked of the whole body rather than of a loop, because both are properties of the body: a `goto` anywhere in it can
/// leave any loop in it, and a coroutine keyword anywhere in it makes the whole body a coroutine.
fn token_facts(body: &cpp_parser::CppCompoundStat) -> (bool, bool) {
    let mut is_a_coroutine = false;
    let mut has_a_goto = false;
    for element in body.syntax().descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        match cpp_parser::CppTokenKind::from(token.kind()) {
            cpp_parser::CppTokenKind::CoAwaitKeyword
            | cpp_parser::CppTokenKind::CoYieldKeyword
            | cpp_parser::CppTokenKind::CoReturnKeyword => is_a_coroutine = true,
            cpp_parser::CppTokenKind::GotoKeyword => has_a_goto = true,
            _ => {}
        }
        if is_a_coroutine && has_a_goto {
            break;
        }
    }
    (is_a_coroutine, has_a_goto)
}

/// **Can any path reach the end of this body?** — the rule's whole control-flow question, asked once.
fn the_body_falls_off_its_end(body: &cpp_parser::CppCompoundStat, has_a_goto: bool) -> bool {
    sequence(body.get_stats(), has_a_goto).reaches
}

/// One statement's flow.
fn flow_of(statement: &CppStat, has_a_goto: bool) -> Flow {
    match statement {
        // A block is its statements, and a `break` inside it is the enclosing statement's.
        CppStat::CompoundStat(block) => sequence(block.get_stats(), has_a_goto),
        // Control does not continue past these. `co_return` parses as a `return`; a coroutine body never gets here.
        CppStat::ReturnStat(_) | CppStat::ThrowStat(_) | CppStat::ContinueStat(_) => Flow::STOPS,
        CppStat::BreakStat(_) => Flow {
            reaches: false,
            breaks: true,
        },
        // **An `if` completes unless both of its branches do not.** A literal condition decides which branch exists,
        // and `if constexpr` with one is the same question one language level up.
        CppStat::IfStat(branch) => {
            let then = branch
                .get_then_branch()
                .map_or(Flow::FALLS_THROUGH, |branch| flow_of(&branch, has_a_goto));
            let other = branch
                .get_else_branch()
                .and_then(|other| other.get_stat())
                .map_or(Flow::FALLS_THROUGH, |branch| flow_of(&branch, has_a_goto));
            match condition_is_true(branch.get_condition()) {
                Some(true) => then,
                Some(false) => other,
                None => Flow {
                    reaches: then.reaches || other.reaches,
                    // **Any path that breaks out makes the enclosing loop leavable**, whichever branch it is on.
                    breaks: then.breaks || other.breaks,
                },
            }
        }
        // **A loop with a condition can be left through it.** An endless one is left only by a `break` of its own
        // (which its body reports) or by a `goto` anywhere in the body.
        CppStat::WhileStat(loop_) => {
            let body = loop_
                .get_body()
                .map_or(Flow::FALLS_THROUGH, |body| flow_of(&body, has_a_goto));
            let endless = condition_is_true(loop_.get_condition()) == Some(true);
            endless_loop(body, endless, has_a_goto)
        }
        CppStat::DoWhileStat(loop_) => {
            let body = loop_
                .get_body()
                .map_or(Flow::FALLS_THROUGH, |body| flow_of(&body, has_a_goto));
            // The condition is tested **after** the body: a body that never finishes never reaches the test, and the
            // loop is left from inside it or not at all.
            if !body.reaches {
                return Flow {
                    reaches: body.breaks || has_a_goto,
                    breaks: false,
                };
            }
            let endless = condition_is_true(loop_.get_condition()) == Some(true);
            endless_loop(body, endless, has_a_goto)
        }
        CppStat::ForStat(loop_) => {
            let body = loop_
                .get_body()
                .map_or(Flow::FALLS_THROUGH, |body| flow_of(&body, has_a_goto));
            let endless = !loop_.has_a_condition();
            endless_loop(body, endless, has_a_goto)
        }
        CppStat::SwitchStat(switch) => switch_flow(switch, has_a_goto),
        // **`try` completes when its block does and every handler does.** A handler may be entered from any call in the
        // block, so the block's own end says nothing on its own — but when both halves stop, no path reaches the
        // statement's end. See [`try_flow`] for what the conservative version cost.
        CppStat::TryStat(try_) => try_flow(try_, has_a_goto),
        // An expression, a declaration, a label: control runs past it.
        _ => Flow::FALLS_THROUGH,
    }
}

/// A loop's flow, given its body's and whether the loop has an exit condition.
///
/// **The loop's own breaks stop here**: `while (true) { while (true) { break; } }` leaves the inner loop only, so the
/// outer one still cannot be left. That is the one place a `break` is consumed rather than reported.
/// **A `try` statement`s flow.** `try` used to be "assumed to complete" — the handler may be entered from any call in
/// the block, so the block`s own end says nothing. That is true and it is not the whole question: a statement whose
/// block stops **and** whose every handler stops cannot be left at all.
///
/// Measured: `corpus/clean/exceptions.cpp` writes three bodies that are one `try` each, with a `return` in the block
/// and in every handler — every path out of them returns — and the walk reported "its body has no `return`" once the
/// rule could see their trailing return type. That is why the trailing type and this are one change.
fn try_flow(try_: &cpp_parser::CppTryStat, has_a_goto: bool) -> Flow {
    let body_of = |body: Option<cpp_parser::CppCompoundStat>| {
        body.map_or(Flow::FALLS_THROUGH, |body| {
            flow_of(&CppStat::CompoundStat(body), has_a_goto)
        })
    };
    let block = body_of(try_.get_body());
    let mut handlers = try_
        .get_catch_handlers()
        .map(|handler| body_of(handler.get_body()));
    // **Both halves are asked separately**: a path that throws inside the block reaches no end of it, and only the
    // handler it lands in says where that path goes.
    let reaches = block.reaches || handlers.any(|handler| handler.reaches);
    let breaks = block.breaks || handlers.any(|handler| handler.breaks);
    Flow { reaches, breaks }
}

fn endless_loop(body: Flow, endless: bool, has_a_goto: bool) -> Flow {
    Flow {
        reaches: !endless || body.breaks || has_a_goto,
        breaks: false,
    }
}

/// A `switch`'s flow: **can it be left through its end?**
///
/// The rule proves only the completion that cannot be doubted:
///
/// ```text
///   no `default:`        a value that matches no label skips the whole body — it may complete
///   a `break` anywhere   the break lands after the switch, which is exactly the completion being asked about
///   a `goto` anywhere    the one exit the statement tree does not model
///   last statement completes with none of those, every path that enters a label runs on (fallthrough) to the last
///                            statement, so its end is the switch's end
/// ```
///
/// Measured on `EmmyLuaCodeStyle`'s `LuaParser::BlockFollow`: four `return`s, a `default`, and the rule called the body
/// one that "has no `return`".
fn switch_flow(switch: &cpp_parser::CppSwitchStat, has_a_goto: bool) -> Flow {
    // **The switch's own children are its body**: the condition, then the labels and statements interleaved
    // (`ParenExpr, CaseStat, ReturnStat, CaseStat, ReturnStat, DefaultStat, ReturnStat`) — this grammar does not put
    // them in a compound block, and looking for one made the whole analysis unreachable.
    let statements: Vec<Flow> = switch
        .syntax()
        .children()
        .filter_map(CppStat::cast)
        .filter(|statement| !matches!(statement, CppStat::CaseStat(_) | CppStat::DefaultStat(_)))
        .map(|statement| flow_of(&statement, has_a_goto))
        .collect();
    let has_default = switch
        .syntax()
        .children()
        .filter_map(CppStat::cast)
        .any(|statement| matches!(statement, CppStat::DefaultStat(_)));

    // **Three ways to complete, and one way not to.** No `default` — some value skips the body. A `break` or a
    // `goto` — there is an exit that lands after the switch. Otherwise the last statement decides: with no `break`,
    // every entered label falls through to it, so its end is the switch's end.
    let reaches = !has_default
        || statements.iter().any(|each| each.breaks)
        || has_a_goto
        || statements.last().is_none_or(|last| last.reaches);

    Flow {
        reaches,
        // A `break` in this switch belongs to it, so it does not leave anything around it.
        breaks: false,
    }
}

/// A sequence of statements: does control reach the point after the last of them?
///
/// **Only reachable statements are followed.** A `break` after a `return` is not a `break` the loop can take, and
/// treating it as one is exactly the wrong direction — it would turn `while (true) { return 1; break; }` into a loop
/// that "can be left", which is a finding about correct code.
fn sequence(statements: impl IntoIterator<Item = CppStat>, has_a_goto: bool) -> Flow {
    let mut flow = Flow::FALLS_THROUGH;
    for statement in statements {
        if !flow.reaches {
            // Unreachable, so it says nothing about control — it is not even walked.
            break;
        }
        let each = flow_of(&statement, has_a_goto);
        flow = Flow {
            reaches: each.reaches,
            breaks: each.breaks,
        };
    }
    flow
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
