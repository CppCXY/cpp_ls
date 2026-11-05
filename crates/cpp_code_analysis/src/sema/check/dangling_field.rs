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
//!
//! # Every question is asked of the tree
//!
//! The member's name, its borrow and its initialiser are typed accessors — `CppDeclaration::get_init_declarator`,
//! `CppDeclarator::{is_a_reference, is_a_pointer, get_name}`, `CppInitializer::get_expr`,
//! `CppMemberInitializer::{get_name, get_argument}`. The borrow used to be a **text slice**: the source between the
//! declaration's start and the member's name, searched for `&`/`*` before the first `<`. That read `std::vector<int*>` as a
//! pointer and a `*` in a default argument as the member's own.

use cpp_parser::CppAstNode;

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
            let Some(body) = cpp_parser::CppClassBody::cast(body) else {
                continue;
            };
            let Some(class_name) = the_class_name(&body) else {
                continue;
            };
            let members = borrowing_members(&body);
            if members.is_empty() {
                continue;
            }

            // **1. An in-class default initialiser.** `std::string_view v = std::string("x");` constructs a temporary for
            // each object and leaves the member pointing at storage that the statement's end frees.
            for declaration in body.get_members() {
                let Some((name, _)) = the_member_type(&declaration) else {
                    continue;
                };
                let Some(borrowing) = members.get(&name) else {
                    continue;
                };
                let Some(initializer) = declaration
                    .get_init_declarator()
                    .and_then(|init| init.get_initializer())
                else {
                    continue;
                };
                let Some(expression) = initializer.get_expr() else {
                    continue;
                };
                // The declaration is its own region: every name in it is a member or a global, and nothing can be a
                // parameter.
                let Some(borrowed) = origin_of(context, expression.syntax(), declaration.syntax())
                else {
                    continue;
                };
                if let Some(message) = the_message(borrowing, &borrowed, &name, "initialised from")
                {
                    let range = cpp_parser::source_range(expression.get_range());
                    context.add(DiagnosticCode::DanglingField, range, name, message);
                }
            }

            // **2. A constructor's initialiser list.** `S(std::string s) : v(s) {}` stores a view of a copy that the
            // constructor destroys.
            for constructor in constructors_of(&body, &class_name) {
                let Some(body_of_the_constructor) = constructor.get_body() else {
                    continue;
                };
                for initializer in member_initialisers_of(&constructor) {
                    let Some(member_name) = initializer.get_name_text() else {
                        continue;
                    };
                    let Some(borrowing) = members.get(&member_name) else {
                        continue;
                    };
                    let Some(argument) = initializer.get_argument() else {
                        continue;
                    };
                    let Some(borrowed) =
                        origin_of(context, argument.syntax(), body_of_the_constructor.syntax())
                    else {
                        continue;
                    };
                    if let Some(message) =
                        the_message(borrowing, &borrowed, &member_name, "initialised from")
                    {
                        let range = cpp_parser::source_range(argument.get_range());
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
fn the_class_name(body: &cpp_parser::CppClassBody) -> Option<String> {
    let class = body
        .syntax()
        .parent()
        .and_then(cpp_parser::CppClassDef::cast)?;
    class.get_name_text()
}

/// **The borrowing members of one class body**, by name. An owning member is not in the map, which is what makes the rest of
/// the rule skip it without a second test.
fn borrowing_members(
    body: &cpp_parser::CppClassBody,
) -> std::collections::BTreeMap<String, Borrowing> {
    let mut members = std::collections::BTreeMap::new();
    for declaration in body.get_members() {
        if let Some((name, borrowing)) = the_member_type(&declaration) {
            members.insert(name, borrowing);
        }
    }
    members
}

/// The member a declaration declares, and the kind of borrowing its type is — `None` for an owning member and for a
/// declaration that is not a member at all (a nested class, a `using`, a function).
fn the_member_type(declaration: &cpp_parser::CppDeclaration) -> Option<(String, Borrowing)> {
    let declarator = declaration.get_init_declarator()?.get_declarator()?;
    let name = declarator.get_name()?.get_name_text().to_string();
    // **The borrow is the declarator's own**: a `&`/`&&` part makes a reference, a `*` part a pointer, and anything else is the
    // type's name, which `view_name_of` classifies by its spelling (`std::string_view`, `std::span`, …).
    let borrowing = if declarator.is_a_reference() {
        Borrowing::Reference
    } else if declarator.is_a_pointer() {
        Borrowing::Pointer
    } else {
        let head = declaration
            .get_decl_specifiers()
            .map(|specifiers| specifiers.get_type_text())
            .unwrap_or_default();
        Borrowing::View(view_name_of(&head)?)
    };
    Some((name, borrowing))
}

/// **The constructors of one class body**: a declaration with a body, no return type, and the class's own name.
///
/// The parser writes `S(std::string s) : v(s) {}` as an outer declaration (the body included) around an inner one (the name, the
/// parameters, the member list), so the outer node is what a caller wants and the inner one is what names it.
fn constructors_of(
    body: &cpp_parser::CppClassBody,
    class_name: &str,
) -> Vec<cpp_parser::CppDeclaration> {
    body.get_members()
        .filter(|declaration| declaration.get_decl_specifiers().is_none())
        .filter(|declaration| declaration.get_init_declarator().is_none())
        .filter(|declaration| declaration.get_body().is_some())
        .filter(|declaration| {
            declaration
                .children::<cpp_parser::CppDeclaration>()
                .any(|inner| inner.get_name_text().as_deref() == Some(class_name))
        })
        .collect()
}

/// **A constructor's `MemberInitializer`s**, wherever the parser put them: the list hangs off the prototype declaration, which
/// the body's declaration wraps.
fn member_initialisers_of(
    constructor: &cpp_parser::CppDeclaration,
) -> Vec<cpp_parser::CppMemberInitializer> {
    let mut found = Vec::new();
    for child in constructor.children::<cpp_parser::CppDeclaration>() {
        found.extend(child.children::<cpp_parser::CppMemberInitializer>());
    }
    found.extend(constructor.children::<cpp_parser::CppMemberInitializer>());
    found
}

/// **The sentence a reader gets**, or `None` when this initialisation is not certainly a dangling one.
fn the_message(
    borrowing: &Borrowing,
    borrowed: &Borrowed,
    member: &str,
    verb: &str,
) -> Option<String> {
    // **Only the two origins that die before the object does.** A reference parameter, a member of `*this`, a static, a global
    // and a string literal all outlive the constructor by the language's own rules.
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
