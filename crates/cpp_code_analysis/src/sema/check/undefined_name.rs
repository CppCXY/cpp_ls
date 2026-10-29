//! **`undefined-name`**: a name read where nothing declares it.
//!
//! ```cpp
//! void f() {
//!     missing.size();     // nothing anywhere declares `missing`
//!     Name n;
//!     n.size();           // …and this is `undefined-field`'s question, not this one
//! }
//! ```
//!
//! # What it asks, and of whom
//!
//! Only about the **object of a member access**, and only when that object is written as one bare name. That is the
//! narrowest place where "nothing declares this" is a fact rather than an unfinished reading:
//!
//! ```text
//!   checked      myName.getFullName(0)     one identifier, whose declaration must exist
//!   not checked  a.b.c                     the object is an expression; the inner `a` is checked instead
//!   not checked  this->size                a keyword, not a name
//!   not checked  std::string::npos         qualified: a namespace lookup, not this question
//!   not checked  (*p).size()               an expression
//! ```
//!
//! # The four reasons to stay quiet
//!
//! Measured against the rule this layer is written against — a wrong squiggle is worse than none, because a reader
//! who is taught to ignore one has turned the whole channel off:
//!
//! ```text
//!   a macro            `#define name value` names it, and the rendering has already expanded it — but a name used
//!                      as a *receiver* may still be the macro's own spelling in a region the walk did not expand
//!   a template         any access inside a `template <…>` is dependent: `T::value`'s members exist only at
//!                      instantiation, and so may an unqualified name inherited from a dependent base
//!   a lambda           a capture is a declaration the scope tree does not bind, so its body is not judged here
//!   an unknown answer  a name in a header nobody read, in a module this project does not contain, or behind a
//!                      condition that could not be evaluated is *not* "not declared" — it is "not read"
//! ```
//!
//! # What counts as declared
//!
//! Two sources, and both have to say no before a finding is added:
//!
//! * **The file's own scopes**, at the offset of the use: a local is visible from its declaration onward, and a class
//!   member is visible throughout the class (a complete-class context), which is why the position test is skipped for
//!   a class scope and applies everywhere else.
//! * **The index**: a declaration in another file is visible, because the files that see this one are its includes —
//!   and a declaration in *this* file is already the scope tree's business, so it is not counted twice (the two
//!   coordinate systems, the rendering's and the file's, are not comparable offset by offset).

use cpp_parser::{CppSyntaxKind, CppTokenKind};

use super::{Check, CheckContext, DiagnosticCode};

/// **`undefined-name`** — see the module documentation.
pub struct UndefinedName;

impl Check for UndefinedName {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UndefinedName];

    fn check(context: &mut CheckContext<'_>) {
        for (node, access) in context.member_accesses() {
            // **One bare identifier**, and nothing else — see the module documentation's table.
            let Some(token) = a_bare_name(&access.object) else {
                continue;
            };
            let name = token.text().to_string();
            let at = usize::from(token.text_range().start());

            // A keyword is not a name: `this->size` and, in some grammars, `super`.
            if matches!(
                CppTokenKind::from(token.kind()),
                CppTokenKind::ThisKeyword
            ) {
                continue;
            }

            if in_a_template(&node) || in_a_lambda(context, at) {
                continue;
            }
            if context
                .summary
                .macros
                .iter()
                .any(|macro_fact| macro_fact.name == name)
            {
                continue;
            }
            if context.model.names_a_template_parameter(&name, at) {
                continue;
            }
            if a_declaration_is_visible(context, &name, at) {
                continue;
            }

            context.add(
                DiagnosticCode::UndefinedName,
                cpp_parser::source_range(token.text_range()),
                name.clone(),
                t!("`%{name}` is not declared: nothing this file can see names it", name = name),
            );
        }
    }
}

/// **The single identifier an expression is written as**, or `None` — see the call site.
fn a_bare_name(object: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxToken> {
    let mut tokens = object
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            !matches!(
                CppTokenKind::from(token.kind()),
                CppTokenKind::Whitespace | CppTokenKind::Newline
            )
        });
    let token = tokens.next()?;
    if tokens.next().is_some() {
        return None;
    }
    Some(token)
}

/// **Is this access inside a `template <…>`?** — see the module documentation on why a dependent context is silent.
///
/// A `template <…>` head is a **sibling** of the declaration it parameterises, not an ancestor of it: the tree is
///
/// ```text
/// Declaration
///   TemplateDecl "template <class T>"        ← the head
///   DeclSpecifierSeq
///     StructDef … ClassBody … count.size     ← the body, one level down
/// ```
///
/// so a walk up the parents finds the body's ancestors and never passes through the head. The question is therefore
/// asked of each ancestor's **declaration**: does the declaration that owns this node also carry a `TemplateDecl`?
fn in_a_template(node: &cpp_parser::CppSyntaxNode) -> bool {
    for ancestor in node.ancestors() {
        if CppSyntaxKind::from(ancestor.kind()) == CppSyntaxKind::TemplateDecl {
            return true;
        }
        let Some(parent) = ancestor.parent() else {
            continue;
        };
        if CppSyntaxKind::from(parent.kind()) == CppSyntaxKind::Declaration
            && parent
                .children()
                .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TemplateDecl)
        {
            return true;
        }
    }
    false
}

/// **Is this offset inside a lambda's body?** — a capture is a declaration the scope tree does not bind.
fn in_a_lambda(context: &CheckContext<'_>, at: usize) -> bool {
    let Some(scope) = context.model.scopes().scope_at(at) else {
        return false;
    };
    context
        .model
        .scopes()
        .scope_chain(scope)
        .iter()
        .any(|id| {
            context
                .model
                .scopes()
                .scope(*id)
                .is_some_and(|data| data.kind == crate::ScopeKind::Lambda)
        })
}

/// **Does anything name `name` where this use is written?**
///
/// `false` only when both sources say no — see the module documentation's two sources, and its note that every other
/// answer means "not read" rather than "not declared".
fn a_declaration_is_visible(context: &CheckContext<'_>, name: &str, at: usize) -> bool {
    let scopes = context.model.scopes();
    if let Some(scope) = scopes.scope_at(at) {
        for id in scopes.scope_chain(scope) {
            let Some(data) = scopes.scope(id) else {
                continue;
            };
            // A class body is a *complete-class context*: everything it declares is visible everywhere inside it,
            // including a member written after the function that uses it. Every other scope is read top to bottom.
            let from_anywhere = matches!(data.kind, crate::ScopeKind::Class | crate::ScopeKind::Enum);
            for binding in &data.bindings {
                if binding.name.identifier_text() != Some(name) {
                    continue;
                }
                if from_anywhere || binding.name_range.start_offset <= at {
                    return true;
                }
            }
        }
    }

    // **A declaration in another file**, which reaches this one through an `#include`. A declaration in *this* file
    // is the scope tree's answer above, in the rendering's coordinates — the index records the file's own offsets,
    // and the two are not comparable one by one once a directive has been expanded.
    match context.model.index().definitions(name, context.path) {
        crate::Known::Yes(found) => found
            .found
            .iter()
            .any(|declaration| declaration.file != context.path),
        // **"Not declared here" is the answer this rule exists for**, and it is an `Unknown` — the index does not
        // claim absence unless it was asked and found nothing. Every other reason is a name this layer cannot speak
        // about: ambiguous (declared twice), behind an unread include, in a module the project does not contain —
        // all of them are silence rather than a finding.
        crate::Known::Unknown(crate::UnknownReason::NotDeclaredHere(_)) => false,
        crate::Known::Unknown(_) => true,
        crate::Known::No => false,
    }
}
