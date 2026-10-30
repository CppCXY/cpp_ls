//! **`dangling-field`** — a member that borrows something which does not live as long as the object.
//!
//! ```cpp
//! struct Dangling {
//!     std::string_view v;                     // a view, not an owner
//!     Dangling(std::string s) : v(s) {}       // `s` is destroyed when the constructor returns
//! };
//!
//! struct Default {
//!     std::string_view v = std::string("x");  // the temporary dies at the end of the statement
//! };
//! ```
//!
//! Both are undefined behaviour by the standard — the object outlives what its member refers to — and both are what
//! Clang's own lifetime analysis calls a *dangling field*. This rule reports them and nothing else:
//!
//! ```text
//!   reported   a view, reference or pointer member initialised from a by-value parameter or a temporary
//!   silent     a reference or pointer parameter (the caller's obligation), a member of `*this`, a static or global,
//!              a string literal, an owning member (there is nothing to see), and anything points-to would be needed for
//! ```
//!
//! # Why a by-value parameter is certain and a reference parameter is not
//!
//! `S(std::string s) : v(s) {}` copies the argument into `s`, and `s` is destroyed at the closing brace of the
//! constructor: the view is dangling before the constructor has returned. `S(const std::string& s) : v(s) {}` stores a
//! view of something the **caller** owns — that is the contract `[[clang::lifetimebound]]` writes down, and whether it
//! holds is a fact about the call site, not about this class. So the rule says nothing about it, exactly as
//! `dangling-return` says nothing about returning a reference parameter.
//!
//! # What it does not see
//!
//! A **pointer** member is reported only when the initialiser is the address of the parameter (`&x`) or a pointer the
//! parameter handed out (`s.c_str()`, `s.data()`): `int* p; S(int* q) : p(q) {}` may be pointing at anything. An
//! assignment in the constructor **body** (`S(std::string s) { v = s; }`) is the same defect and is not reported yet —
//! it needs the same origin analysis over statement expressions, which is the next step for this rule.

use cpp_parser::CppAstNode;
use cpp_parser::CppSyntaxKind;

use super::borrowed::{Borrowed, Origin, origin_of, view_name_of};
use super::{Check, CheckContext, DiagnosticCode};

pub struct DanglingField;

impl Check for DanglingField {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::DanglingField];

    fn check(context: &mut CheckContext<'_>) {
        let bodies: Vec<cpp_parser::CppSyntaxNode> = context
            .own::<cpp_parser::CppClassBody>()
            .map(|body| body.syntax().clone())
            .collect();

        for body in bodies {
            let Some(class_name) = the_class_name(&body) else {
                continue;
            };
            let members = borrowing_members(context, &body);
            if members.is_empty() {
                continue;
            }

            // **1. An in-class default initialiser.** `std::string_view v = std::string("x");` constructs a
            // temporary for each object and leaves the member pointing at storage that the statement's end frees.
            for declaration in direct_children_of_kind(&body, CppSyntaxKind::Declaration) {
                let Some((name, _)) = the_member_type(context, &declaration) else {
                    continue;
                };
                let Some(borrowing) = members.get(&name) else {
                    continue;
                };
                let Some(initializer) = declaration
                    .children()
                    .find(|child| kind(child) == CppSyntaxKind::InitDeclarator)
                    .and_then(|init| {
                        init.children()
                            .find(|child| kind(child) == CppSyntaxKind::Initializer)
                    })
                else {
                    continue;
                };
                let Some(expression) = initializer.children().next() else {
                    continue;
                };
                // The declaration is its own region: every name in it is a member or a global, and nothing can be a
                // parameter.
                let Some(borrowed) = origin_of(context, &expression, &declaration) else {
                    continue;
                };
                if let Some(message) = the_message(borrowing, &borrowed, &name, "initialised from")
                {
                    let range = cpp_parser::source_range(expression.text_range());
                    context.add(DiagnosticCode::DanglingField, range, name, message);
                }
            }

            // **2. A constructor's initialiser list.** `S(std::string s) : v(s) {}` stores a view of a copy that the
            // constructor destroys.
            for constructor in constructors_of(&body, &class_name) {
                let Some(body_of_the_constructor) = constructor
                    .children()
                    .find(|child| kind(child) == CppSyntaxKind::CompoundStat)
                else {
                    continue;
                };
                // **The initialiser list belongs to the constructor's *prototype*.** The parser writes the ctor as an
                // outer declaration (body included) around an inner one (name, parameters, `: v(s)`), so the
                // `MemberInitializer`s are one level down — measured on `S(std::string s) : v(s) {}`.
                for initializer in member_initialisers_of(&constructor) {
                    let Some(member_name) = initializer
                        .children()
                        .find(|child| kind(child) == CppSyntaxKind::NameExpr)
                        .map(|name| name.text().to_string())
                    else {
                        continue;
                    };
                    let Some(borrowing) = members.get(&member_name) else {
                        continue;
                    };
                    let Some(argument) = the_initialiser_expression(&initializer) else {
                        continue;
                    };
                    let Some(borrowed) = origin_of(context, &argument, &body_of_the_constructor)
                    else {
                        continue;
                    };
                    if let Some(message) =
                        the_message(borrowing, &borrowed, &member_name, "initialised from")
                    {
                        let range = cpp_parser::source_range(argument.text_range());
                        context.add(DiagnosticCode::DanglingField, range, member_name, message);
                    }
                }
            }
        }
    }
}

/// **What kind of borrowing a member's type is** — the three shapes a lifetime rule distinguishes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Borrowing {
    /// `std::string_view`, `std::span<T>`, …: a non-owning view.
    View(String),
    /// `const std::string&`: a reference member.
    Reference,
    /// `const char*`: a pointer, reported only when the initialiser is certainly this object's own address.
    Pointer,
}

/// The class a body belongs to, for recognising its constructors.
fn the_class_name(body: &cpp_parser::CppSyntaxNode) -> Option<String> {
    let class = body.parent()?;
    if !matches!(
        kind(&class),
        CppSyntaxKind::ClassDef | CppSyntaxKind::StructDef | CppSyntaxKind::UnionDef
    ) {
        return None;
    }
    class
        .children()
        .find(|child| kind(child) == CppSyntaxKind::NameExpr)
        .map(|name| name.text().to_string())
}

/// **The borrowing members of one class body**, by name. An owning member is not in the map, which is what makes the
/// rest of the rule skip it without a second test.
fn borrowing_members(
    context: &CheckContext<'_>,
    body: &cpp_parser::CppSyntaxNode,
) -> std::collections::BTreeMap<String, Borrowing> {
    let mut members = std::collections::BTreeMap::new();
    for declaration in direct_children_of_kind(body, CppSyntaxKind::Declaration) {
        if let Some((name, borrowing)) = the_member_type(context, &declaration) {
            members.insert(name, borrowing);
        }
    }
    members
}

/// The member a declaration declares, and the kind of borrowing its type is — `None` for an owning member and for a
/// declaration that is not a member at all (a nested class, a `using`, a function).
fn the_member_type(
    context: &CheckContext<'_>,
    declaration: &cpp_parser::CppSyntaxNode,
) -> Option<(String, Borrowing)> {
    let declarator = declaration
        .children()
        .find(|child| kind(child) == CppSyntaxKind::InitDeclarator)
        .and_then(|init| {
            init.children()
                .find(|child| kind(child) == CppSyntaxKind::Declarator)
        })?;
    // The name is a **direct** child of the declarator: the `NameExpr` inside a parameter list belongs to a function,
    // and a member's type is what is written before its own name.
    let name = declarator
        .children()
        .find(|child| kind(child) == CppSyntaxKind::NameExpr)?;
    let start = usize::from(declaration.text_range().start());
    let name_start = usize::from(name.text_range().start());
    let prefix = context.source.get(start..name_start)?;
    // The `&`/`*` are asked about **before the first `<`**, so `std::vector<int*>` is a vector rather than a pointer.
    let head = prefix.split('<').next().unwrap_or_default();
    let borrowing = if head.contains('&') {
        Borrowing::Reference
    } else if head.contains('*') {
        Borrowing::Pointer
    } else {
        Borrowing::View(view_name_of(head)?)
    };
    Some((name.text().to_string(), borrowing))
}

/// The constructors of one class body: a declaration with a body, no return type, and the class's own name.
fn constructors_of(
    body: &cpp_parser::CppSyntaxNode,
    class_name: &str,
) -> Vec<cpp_parser::CppSyntaxNode> {
    direct_children_of_kind(body, CppSyntaxKind::Declaration)
        .into_iter()
        .filter(|declaration| {
            declaration.children().all(|child| {
                kind(&child) != CppSyntaxKind::DeclSpecifierSeq
                    && kind(&child) != CppSyntaxKind::InitDeclarator
            }) && declaration
                .children()
                .any(|child| kind(&child) == CppSyntaxKind::CompoundStat)
                && declaration.children().any(|child| {
                    kind(&child) == CppSyntaxKind::Declaration
                        && the_declared_name(&child).as_deref() == Some(class_name)
                })
        })
        .collect()
}

/// The name a declaration declares, from its own declarator — `S` for `S(std::string s)`.
fn the_declared_name(declaration: &cpp_parser::CppSyntaxNode) -> Option<String> {
    declaration
        .children()
        .find(|child| kind(child) == CppSyntaxKind::Declarator)
        .and_then(|declarator| {
            declarator
                .children()
                .find(|child| kind(child) == CppSyntaxKind::NameExpr)
        })
        .map(|name| name.text().to_string())
}

/// **A constructor's `MemberInitializer`s**, wherever the parser put them: the list hangs off the prototype
/// declaration, which the body's declaration wraps.
fn member_initialisers_of(
    constructor: &cpp_parser::CppSyntaxNode,
) -> Vec<cpp_parser::CppSyntaxNode> {
    let mut found = Vec::new();
    for child in constructor.children() {
        if kind(&child) == CppSyntaxKind::Declaration {
            found.extend(direct_children_of_kind(
                &child,
                CppSyntaxKind::MemberInitializer,
            ));
        } else if kind(&child) == CppSyntaxKind::MemberInitializer {
            found.push(child);
        }
    }
    found
}

/// The expression a member-initialiser initialises with — `s` in `: v(s)`, and the braced list in `: v{s}`.
fn the_initialiser_expression(
    initializer: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    for child in initializer.children() {
        match kind(&child) {
            CppSyntaxKind::NameExpr => continue,
            // `(s)` and `{s}` are both an initialiser: the argument list holds the expression, and the braced list is
            // one already.
            _ => {
                return match kind(&child) {
                    CppSyntaxKind::ArgumentList | CppSyntaxKind::Initializer => {
                        child.children().next()
                    }
                    _ => Some(child),
                };
            }
        }
    }
    None
}

/// **The sentence a reader gets**, or `None` when this initialisation is not certainly a dangling one.
fn the_message(
    borrowing: &Borrowing,
    borrowed: &Borrowed,
    member: &str,
    verb: &str,
) -> Option<String> {
    // **Only the two origins that die before the object does.** A reference parameter, a member of `*this`, a static, a
    // global and a string literal all outlive the constructor by the language's own rules.
    if !matches!(borrowed.kind, Origin::ByValueParameter | Origin::Temporary) {
        return None;
    }
    let described = match borrowing {
        Borrowing::View(view) => view.as_str(),
        Borrowing::Reference => "reference",
        Borrowing::Pointer => "pointer",
    };
    match (borrowing, borrowed.kind) {
        (Borrowing::Pointer, Origin::ByValueParameter)
            if !borrowed.address_of && borrowed.through.is_none() =>
        {
            // `int* p; S(int* q) : p(q) {}` may point at anything.
            None
        }
        (Borrowing::Pointer, Origin::Temporary)
            if !borrowed.address_of && borrowed.through.is_none() =>
        {
            None
        }
        (_, Origin::ByValueParameter) => Some(format!(
            "`{member}` is a `{described}` {verb} `{name}`, a parameter that is destroyed when the constructor returns",
            name = borrowed.description
        )),
        (Borrowing::Pointer, Origin::Temporary) => Some(format!(
            "`{member}` is a pointer {verb} a temporary, which is destroyed at the end of the statement",
        )),
        (_, Origin::Temporary) => Some(format!(
            "`{member}` is a `{described}` {verb} a temporary, which is destroyed at the end of the statement",
        )),
        _ => None,
    }
}

fn direct_children_of_kind(
    node: &cpp_parser::CppSyntaxNode,
    wanted: CppSyntaxKind,
) -> Vec<cpp_parser::CppSyntaxNode> {
    node.children()
        .filter(|child| kind(child) == wanted)
        .collect()
}

fn kind(node: &cpp_parser::CppSyntaxNode) -> CppSyntaxKind {
    CppSyntaxKind::from(node.kind())
}
