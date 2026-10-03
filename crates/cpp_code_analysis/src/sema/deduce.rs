//! **What an `auto` stands for** — the type a declaration did not write.
//!
//! A declaration whose type is a placeholder is the one kind whose type is not in the file at all: `auto x = f();`
//! says `x` is whatever `f` returns, and nothing in that line spells it. So this module answers a question the
//! reader of a single declaration cannot, and it answers it **only from what the file itself says** — an
//! initializer's own expression, typed by the engine [`type_of_expression`] already provides.
//!
//! # What it deduces, and what it refuses
//!
//! Measured over MSVC's headers, `auto` declarations divide by what follows the name:
//!
//! ```text
//!   111  a call                        `auto x = f(a, b);`      the callee's return type
//!    83  a plain expression            `auto x = y;`            the name's own type
//!    63  a cast                        `auto x = static_cast<T>(v);`
//!    18  no initializer                `auto f() { … }`         the return statements, not the declaration
//!     8  a braced initializer          `auto x{…};`
//! ```
//!
//! The first three are one question — *what is this expression's type* — and they are what this module answers.
//! The last two are **not**, and they are refused rather than approximated:
//!
//! * `auto f() { … }` has no initializer at all; its type is what the `return` statements agree on, which is a
//!   second walk over the function's body and a different question about agreement. Answering it with the first
//!   `return` would be wrong whenever the others differ, and a signature is not the place to be wrong.
//! * `auto x{…};` deduces `initializer_list<T>` or `T` depending on the braces, which is a rule about the
//!   *initializer* rather than about the expression inside it.
//!
//! # The refusal is part of the contract
//!
//! [`Known::Unknown`] is the answer whenever the initializer's type is not one this can read, and it carries
//! **why** — the name is not declared here, the expression is inside a macro this analysis did not expand, the
//! type depends on a template argument. A consumer that shows `auto` for those is showing what the file says; a
//! consumer that invented a type would be showing something the file does not.

use std::path::Path;

use cpp_parser::{CppSyntaxKind, CppSyntaxNode};

use crate::DeclFact;
use crate::ScopeTree;
use crate::index::ProjectIndex;
use crate::sema::symbol::{Known, UnknownReason};

/// **The type an `auto` declaration's initializer has**, or why it cannot be told.
///
/// `Known::No` is the answer for a declaration whose type is **not** a placeholder: the question does not apply
/// to it, which is not the same as an answer that could not be found. A caller asking about every declaration in
/// a file — which is what a consumer walking a summary does — gets `No` for most of them, and that is the
/// distinction [`Known`] exists to keep.
pub fn deduced_type_of(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    fact: &DeclFact,
) -> Known<String> {
    // **A function's placeholder is not in `type_of`.** `auto f() { … }` has no type of its own — the fact's
    // `type_of` is `None` and its **`returns`** is the `auto`. Asked separately because the answer is a different
    // question: a variable's type is its initializer's, while a function's is what its `return` statements agree
    // on, and agreeing is a walk this does not do. Refused with that reason rather than reported as "the question
    // does not apply", which would say the declaration has no placeholder at all.
    if fact.returns.as_deref() == Some("auto") {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(
            "a deduced return type — the return statements, not an initializer",
        )));
    }

    if fact.type_of.as_deref() != Some("auto") {
        return Known::No;
    }

    let Some(initializer) = initializer_of(root, fact.range) else {
        // `auto x;` with nothing after the name. The type is not in the file at all, and no walk of it would find
        // one — the declaration is incomplete rather than deducible.
        return Known::Unknown(UnknownReason::UnknownType(Box::from("auto without an initializer")));
    };

    let Some(expression) = the_expression_in(&initializer) else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from("auto{}")));
    };

    match crate::index::project::type_of_expression(index, scopes, root, path, &expression, 0) {
        Known::Yes((type_of, _)) => Known::Yes(type_of.to_string()),
        Known::Unknown(reason) => Known::Unknown(reason),
        // The expression engine says the question does not apply to what it was handed — which for an initializer
        // that was found and read means the reading is not one it recognises.
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(
            expression.text().to_string().trim(),
        ))),
    }
}

/// The `Initializer` node written inside `declaration`, if there is one.
///
/// A declaration's initializer is a node of its own — `= expr`, `(expr)` or `{…}` — so finding it is a search of
/// the declaration's own subtree rather than a scan for an `=` token: the `=` of a default template argument, of
/// a `requires` clause or of an operator name is not this declaration's initializer, and a node cannot be
/// confused with one.
fn initializer_of(root: &CppSyntaxNode, declaration: cpp_parser::SourceRange) -> Option<CppSyntaxNode> {
    root.descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)
        .find(|node| {
            let range = cpp_parser::source_range(node.text_range());
            range.start_offset >= declaration.start_offset && range.end_offset() <= declaration.end_offset()
        })
}

/// **The expression an initializer holds** — its last child that is a node rather than a token.
///
/// `= f(x)` holds the `=` and the call; `{1, 2}` holds the braces and the list. The last node is the payload in
/// both, and a braced list comes back as itself, which the module note says is refused rather than read.
fn the_expression_in(initializer: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    initializer.children().last()
}
