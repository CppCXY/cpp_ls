//! **`use-after-free`** — memory that was released and is then read.
//!
//! ```cpp
//! void f() {
//!     T* p = new T();
//!     delete p;
//!     p->x;              // the object is gone
//! }
//!
//! void release(T* p) { delete p; }        // in this file, or in a header
//! void g() {
//!     T* p = new T();
//!     release(p);
//!     p->x;              // `release` frees its parameter — recorded on the declaration's fact
//! }
//! ```
//!
//! # What counts as a free
//!
//! ```text
//!   delete p;  delete[] p;        the language's own
//!   free(p)                       C, and C++ via <cstdlib>
//!   std::destroy_at(p)            the object's lifetime ends
//!   u.reset()                     the pointee of a smart pointer is gone; `u` itself is not
//!   release(p)                    a call whose callee's fact says it frees that parameter
//! ```
//!
//! The last one is the inter-procedural half, and it is why [`DeclFact::frees`](crate::DeclFact::frees) exists: a call
//! site cannot see the callee's body, and this layer must not parse one — so the fact carries the answer, decided once
//! when the callee was read. Only an **unconditional** free in the callee is recorded, so a `delete` under an `if`
//! there cannot make a caller's use look like a defect.
//!
//! # Certainty, and what is therefore silent
//!
//! The analysis is a **must**-analysis: a variable is freed after a statement only if every path to that point freed
//! it. So a free in one branch of an `if` does not make a later use a finding, a free inside a loop does not escape the
//! loop, and a variable that is reassigned loses its freed state. Both of the last two are the language's own
//! semantics rather than an approximation: `delete p; p = new T();` is correct code.
//!
//! Uses are counted where the **pointee** is read: `p->x`, `*p`, `p[i]`. A `.` is not — `u.reset()` accesses its own
//! object, and a smart pointer after `reset` is null rather than dangling.
//!
//! # What it does not see
//!
//! A pointer freed through a **conditional** callee, a freed pointer stored in a field or returned, `delete` on a
//! member (`this->p` — the identity is the object, not the field), double frees, and container invalidation. Each is a
//! rule of its own, and the last one belongs to a check that is off by default.

use std::collections::BTreeSet;

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

pub struct UseAfterFree;

impl Check for UseAfterFree {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UseAfterFree];

    fn check(context: &mut CheckContext<'_>) {
        // **One walk per function body.** A `return`, a closure's body and the bodies of the class's members are
        // separate regions, and a freed variable is not visible outside the one it was declared in.
        let bodies: Vec<cpp_parser::CppSyntaxNode> = context
            .own::<cpp_parser::CppDeclaration>()
            .filter_map(|declaration| declaration.get_body())
            .map(|body| body.syntax().clone())
            .collect();

        for body in bodies {
            let mut freed: Freed = BTreeSet::new();
            walk(&body, &mut freed, context);
        }
    }
}

/// The variables whose pointee has been freed, **by the offset of the declaration's name** — the one identity a name
/// has that survives being copied into an expression.
type Freed = BTreeSet<usize>;

/// Walk one statement, carrying the state of every variable freed so far.
fn walk(statement: &cpp_parser::CppSyntaxNode, freed: &mut Freed, context: &mut CheckContext<'_>) {
    use cpp_parser::CppAstNode;

    // **The statement sum type**, not a kind: a node that is not a statement keeps the old behaviour (its uses count, its effects
    // are not a statement's).
    let Some(statement_node) = cpp_parser::CppStat::cast(statement.clone()) else {
        check_uses(statement, freed, context);
        return;
    };
    match &statement_node {
        // A block and an `else` are containers of statements rather than statements themselves.
        cpp_parser::CppStat::CompoundStat(_) | cpp_parser::CppStat::ElseStat(_) => {
            for child in statement.children() {
                walk(&child, freed, context);
            }
        }
        // **What happens inside a branch does not decide what happens after it.** The condition and the branches are
        // walked with a copy, so a use inside them is reported against the state they see, and a free they perform
        // does not escape — a free under an `if` may not run.
        cpp_parser::CppStat::IfStat(_)
        | cpp_parser::CppStat::ForStat(_)
        | cpp_parser::CppStat::WhileStat(_)
        | cpp_parser::CppStat::DoWhileStat(_)
        | cpp_parser::CppStat::SwitchStat(_)
        | cpp_parser::CppStat::TryStat(_) => {
            let mut inside = freed.clone();
            for child in statement.children() {
                walk(&child, &mut inside, context);
            }
        }
        cpp_parser::CppStat::ExpressionStat(_)
        | cpp_parser::CppStat::ReturnStat(_)
        | cpp_parser::CppStat::Declaration(_) => {
            check_uses(statement, freed, context);
            apply_effects(statement, freed, context);
        }
        // Anything else is an expression in a position the arms above did not name — its uses count, its effects are not a
        // statement's.
        _ => check_uses(statement, freed, context),
    }
}

/// **Reads of a freed pointee.** Only the three spellings that read through the pointer count: `p->x`, `*p`, `p[i]`.
fn check_uses(
    statement: &cpp_parser::CppSyntaxNode,
    freed: &Freed,
    context: &mut CheckContext<'_>,
) {
    for candidate in statement.descendants() {
        // **The three spellings that read through the pointer**, asked of the expression rather than of its tokens: `p->x` is
        // an arrow member access, `p[i]` a subscript, and `*p` a unary `*`. (`p.x` is not a read *through* the pointer, and the
        // arrow test is what keeps it out.)
        let (base, reads_through_it) = match cpp_parser::CppExpr::cast(candidate.clone()) {
            Some(cpp_parser::CppExpr::MemberExpr(member)) => {
                let Some(base) = member.get_object() else {
                    continue;
                };
                (base.syntax().clone(), member.is_arrow())
            }
            Some(cpp_parser::CppExpr::IndexExpr(index)) => {
                let Some(base) = index.get_object() else {
                    continue;
                };
                // `a[i]` reads through `a`; `a.b` only when the operator is `->` — an `a.b` is an `IndexExpr` too (measured), so
                // the arrow is asked of the access and not assumed from the node.
                let reads = if index.is_a_subscript() {
                    true
                } else {
                    index
                        .syntax()
                        .children_with_tokens()
                        .filter_map(|element| element.into_token())
                        .any(|token| {
                            cpp_parser::CppTokenKind::from(token.kind())
                                == cpp_parser::CppTokenKind::Arrow
                        })
                };
                (base.syntax().clone(), reads)
            }
            Some(cpp_parser::CppExpr::UnaryExpr(unary)) => {
                let Some(base) = unary.get_operand() else {
                    continue;
                };
                (
                    base.syntax().clone(),
                    unary.get_operator_text().as_deref() == Some("*"),
                )
            }
            _ => continue,
        };
        if !reads_through_it {
            continue;
        }
        let Some((offset, name)) = the_variable(&base, context) else {
            continue;
        };
        if !freed.contains(&offset) {
            continue;
        }
        let range = cpp_parser::source_range(base.text_range());
        context.add(
            DiagnosticCode::UseAfterFree,
            range,
            name.clone(),
            format!("`{name}` is used after the memory it refers to was freed"),
        );
    }
}

/// **Updates the state from one statement's own effects**, in document order.
fn apply_effects(
    statement: &cpp_parser::CppSyntaxNode,
    freed: &mut Freed,
    context: &mut CheckContext<'_>,
) {
    for candidate in statement.descendants() {
        // **The three effects, asked of the expression**: a `delete`, a call that frees, and an assignment.
        let Some(expression) = cpp_parser::CppExpr::cast(candidate.clone()) else {
            continue;
        };
        match expression {
            // `delete p;` and `delete[] p;` — the operator is the expression's own.
            cpp_parser::CppExpr::UnaryExpr(unary) if unary.is_delete() => {
                if let Some(operand) = unary.get_operand()
                    && let Some((offset, _)) = the_variable(operand.syntax(), context)
                {
                    freed.insert(offset);
                }
            }
            cpp_parser::CppExpr::CallExpr(call) => {
                let Some(callee) = call.get_callee() else {
                    continue;
                };
                let arguments: Vec<cpp_parser::CppExpr> = call.get_args();
                let name = the_last_identifier_of(callee.syntax());
                let receiver = a_member_access_object(&callee);
                match name.as_deref() {
                    // `free(p)` — C, and C++ through `<cstdlib>`.
                    Some("free") => {
                        if let Some(argument) = arguments.first()
                            && let Some((offset, _)) = the_variable(argument.syntax(), context)
                        {
                            freed.insert(offset);
                        }
                    }
                    // `std::destroy_at(p)` — the object's lifetime ends, and a later read of it is a use after its lifetime,
                    // which is the same defect with a different name.
                    Some("destroy_at") => {
                        if let Some(argument) = arguments.first()
                            && let Some((offset, _)) = the_variable(argument.syntax(), context)
                        {
                            freed.insert(offset);
                        }
                    }
                    // `u.reset()` — the pointee is gone; `u` itself stays valid.
                    Some("reset") => {
                        if let Some(receiver) = receiver
                            && let Some((offset, _)) = the_variable(receiver.syntax(), context)
                        {
                            freed.insert(offset);
                        }
                    }
                    // **A call whose callee's fact says it frees this argument.** The bit is per parameter index, decided when
                    // the callee was read — see [`crate::DeclFact::frees`].
                    _ => {
                        if let Some(frees) = the_frees_of(context, callee.syntax())
                            && frees != 0
                        {
                            for (index, argument) in arguments.iter().enumerate() {
                                if index < 64
                                    && frees & (1 << index) != 0
                                    && let Some((offset, _)) =
                                        the_variable(argument.syntax(), context)
                                {
                                    freed.insert(offset);
                                }
                            }
                        }
                    }
                }
            }
            // `p = …` — a variable that is assigned a value is no longer the one that was freed, unless the value is another
            // freed variable.
            cpp_parser::CppExpr::BinaryExpr(binary)
                if binary.get_operator_text().as_deref() == Some("=") =>
            {
                let Some(target) = binary.get_lhs() else {
                    continue;
                };
                // **The right-hand side as a node**: `p = new T()` is an assignment whose value is a `new` expression, and
                // `get_rhs` answers an `Option<CppExpr>` — which every expression is now, but the variable test wants the node.
                let Some(value) = binary.syntax().children().nth(1) else {
                    continue;
                };
                let Some((offset, _)) = the_variable(target.syntax(), context) else {
                    continue;
                };
                // Assigning another freed variable keeps the state; anything else — a `new`, a call, a literal — is a fresh value
                // this analysis knows nothing bad about.
                match the_variable(&value, context) {
                    Some((source, _)) if freed.contains(&source) => {
                        freed.insert(offset);
                    }
                    _ => {
                        freed.remove(&offset);
                    }
                }
            }
            _ => {}
        }
    }
}

/// **The fact's effect summary for a callee** — this file's summary first, then the index.
///
/// Two ways in, because the name does not always resolve: a helper declared in a header is answered by the **index's**
/// include-visibility rule from the spelling alone, which is the same question `definition_at` asks of a reading that
/// has the header inlined.
fn the_frees_of(context: &CheckContext<'_>, callee: &cpp_parser::CppSyntaxNode) -> Option<u64> {
    if let Some(offset) = the_last_identifier_offset(callee)
        && let crate::Known::Yes(binding) =
            crate::sema::resolve::definition_at(context.scopes(), context.tree(), offset)
    {
        let name = binding.name.text();
        let scope = context.scopes().qualified_name_of(binding.scope);
        // **This file's summary first** — the facts the reading already produced, and the callee is usually here.
        if let Some(fact) = context.summary.declarations.iter().find(|fact| {
            fact.name == name && fact.scope == scope && fact.kind == crate::DeclKind::Function
        }) {
            return Some(fact.frees);
        }
        let spelling = match scope.as_deref() {
            Some(scope) if !scope.is_empty() => format!("{scope}::{name}"),
            _ => name,
        };
        if let crate::Known::Yes(found) = context.index().definition(&spelling, context.path) {
            return Some(found.fact.frees);
        }
    }
    // **The spelling alone**, for a helper the reading cannot resolve. Only a free function: a member call's name is
    // not a name any header declares, and asking by it would answer a different function's effects.
    if is_a_member_access(callee) {
        return None;
    }
    let name = the_last_identifier_of(callee)?;
    match context.index().definition(&name, context.path) {
        crate::Known::Yes(found) => Some(found.fact.frees),
        _ => None,
    }
}
/// **The local variable an expression names**, with the offset its declaration's name starts at.
///
/// `None` for anything else — a member (`this->p`), a global, a parameter of another function, a temporary. Only a
/// local or a parameter is a thing this analysis can say was freed: the state is per *declaration in this body*.
fn the_variable(
    expression: &cpp_parser::CppSyntaxNode,
    context: &CheckContext<'_>,
) -> Option<(usize, String)> {
    let expression = skip_parentheses(expression);
    match cpp_parser::CppExpr::cast(expression.clone()) {
        // `&x` names `x`: `destroy_at(&x)` destroys the object `x` is.
        Some(cpp_parser::CppExpr::UnaryExpr(unary)) => {
            the_variable(unary.get_operand()?.syntax(), context)
        }
        Some(cpp_parser::CppExpr::NameExpr(_)) => {
            let offset = the_last_identifier_offset(&expression)?;
            let crate::Known::Yes(binding) =
                crate::sema::resolve::definition_at(context.scopes(), context.tree(), offset)
            else {
                return None;
            };
            let scope = context.scopes().scope(binding.scope)?;
            if !matches!(
                scope.kind,
                crate::ScopeKind::Function | crate::ScopeKind::Block | crate::ScopeKind::Lambda
            ) {
                return None;
            }
            Some((binding.name_range.start_offset, binding.name.text()))
        }
        _ => None,
    }
}

fn skip_parentheses(expression: &cpp_parser::CppSyntaxNode) -> cpp_parser::CppSyntaxNode {
    if !cpp_parser::CppParenExpr::can_cast(expression.kind().into()) {
        return expression.clone();
    }
    expression
        .children()
        .next()
        .map(|inner| skip_parentheses(&inner))
        .unwrap_or_else(|| expression.clone())
}

fn is_a_member_access(expression: &cpp_parser::CppSyntaxNode) -> bool {
    expression
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            matches!(
                cpp_parser::CppTokenKind::from(token.kind()),
                cpp_parser::CppTokenKind::Dot | cpp_parser::CppTokenKind::Arrow
            )
        })
}

fn the_last_identifier_offset(expression: &cpp_parser::CppSyntaxNode) -> Option<usize> {
    expression
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::Identifier
        })
        .last()
        .map(|token| usize::from(token.text_range().start()))
}

fn the_last_identifier_of(expression: &cpp_parser::CppSyntaxNode) -> Option<String> {
    expression
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::Identifier
        })
        .last()
        .map(|token| token.text().to_string())
}

/// **The object of a member access** — `u` for `u.reset()`: an `a.b` is an `IndexExpr` (measured) and an `a->b` may be a
/// `MemberExpr`; both answer their object.
fn a_member_access_object(expression: &cpp_parser::CppExpr) -> Option<cpp_parser::CppExpr> {
    match expression {
        cpp_parser::CppExpr::MemberExpr(member) => member.get_object(),
        cpp_parser::CppExpr::IndexExpr(index) if index.is_a_member_access() => index.get_object(),
        _ => None,
    }
}
