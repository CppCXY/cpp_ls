//! **`access-invisible`**: a member reached from somewhere that may not reach it.
//!
//! ```cpp
//! class Widget {
//!     int secret;              // private: `class` starts private
//! public:
//!     int size;
//! };
//!
//! Widget w;
//! w.size;      // fine
//! w.secret;    // `secret` is private and this is not Widget's own code
//! ```
//!
//! # Only this file, and that is the whole scope of the check
//!
//! Both sides of the question have to be **in the file being checked**:
//!
//! ```text
//!   the member        declared by a class whose definition is here, so that its access specifiers can be read
//!   the access        written here, which is where the reader is looking
//! ```
//!
//! A member of a class from a header is **not** judged — not even when the header has been read and the access is
//! plainly private. A class's accessibility is a fact about a **class**, and a class is defined once and included
//! everywhere; a check that read one from another file would be answering about that file's text in this one, and
//! the answer would be wrong for every translation unit that sees a different definition. The same reason the
//! `#include`-level checks stay out of headers runs the other way here: **this analysis checks the file it is
//! given**.
//!
//! # How the two facts are found
//!
//! The member's declaration is a node in this file's tree, found by the range the member list reports — so the
//! class that holds it is found by walking **up** from it, whichever node kind the grammar gave the class. The
//! access specifier in force at a member is then found by reading the class body's text **in order**: the last
//! `public:`/`protected:`/`private:` written before the member, or the class's own default (`class` is private,
//! `struct` is public).
//!
//! Read as text rather than as nodes because a class body is where a checker is most likely to meet a construct the
//! grammar recovered around, and because the question — "which of the three labels is above this line" — is a
//! question about lines.

use cpp_parser::{CppSyntaxKind, CppSyntaxNode};

use super::{Check, CheckContext, DiagnosticCode};

/// **`access-invisible`** — see the module documentation.
pub struct AccessInvisible;

/// Which of the three labels is in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Public,
    Protected,
    Private,
}

impl Access {
    /// The name a message spells — the keyword the reader wrote.
    fn keyword(self) -> &'static str {
        match self {
            Access::Public => "public",
            Access::Protected => "protected",
            Access::Private => "private",
        }
    }
}

impl Check for AccessInvisible {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::AccessInvisible];

    fn check(context: &mut CheckContext<'_>) {
        for node in context
            .tree()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MemberExpr)
        {
            let Some(access) = crate::sema::resolve::member_access_of(&node) else {
                continue;
            };
            if access.member.is_empty() {
                continue;
            }

            let crate::Known::Yes((object, _)) = context.type_of(&access.object) else {
                continue;
            };
            let crate::Known::Yes(members) = context.model.members_of(&object.class_name().unwrap_or_default())
            else {
                continue;
            };
            let Some(member) = members
                .members
                .iter()
                .find(|member| member.fact.name == access.member)
            else {
                // Not this rule's question: a name that is not there is `undefined-field`'s.
                continue;
            };

            // **The declaration of that member, in this file** — found by the range the member list reports, which
            // is the name's. A member declared in another file is not here, and this check does not judge it.
            let Some(declaration) = node_at(context.tree(), member.fact.range.start_offset) else {
                continue;
            };
            let Some(class) = enclosing_definition(&declaration) else {
                continue;
            };

            let visibility = visibility_of(&class, member.fact.range.start_offset);
            if visibility == Access::Public {
                continue;
            }

            // **The access is inside the class that declares it**: a member function of `Widget` may read
            // `Widget::secret`. Anything else — another class, a free function, a nested lambda outside — may not.
            if enclosing_definition(&node).is_some_and(|holder| same_node(&holder, &class)) {
                continue;
            }

            context.add(
                DiagnosticCode::AccessInvisible,
                access.member_range,
                access.member.clone(),
                format!(
                    "`{}` is {} in the class that declares it, and this is not that class's own code",
                    access.member,
                    visibility.keyword()
                ),
            );
        }
    }
}

/// The innermost node whose range **starts** at this offset, which is what turns a member's name range into the
/// declaration that holds it.
fn node_at(root: &CppSyntaxNode, offset: usize) -> Option<CppSyntaxNode> {
    root.descendants()
        .filter(|node| usize::from(node.text_range().start()) == offset)
        .last()
}

/// The nearest ancestor that is a **definition** — a class, a struct or a union body.
///
/// Recognised by the text of its own head rather than by a node kind, because the grammar's names for these are an
/// implementation detail and the question ("is this inside a class definition") is not.
fn enclosing_definition(node: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        let text = ancestor.text().to_string();
        let head = text.trim_start();
        if (head.starts_with("class ")
            || head.starts_with("struct ")
            || head.starts_with("union ")
            || head.starts_with("enum "))
            && text.contains('{')
        {
            return Some(ancestor);
        }
        current = ancestor.parent();
    }
    None
}

/// Whether two handles are the same node — by range, because a node is a handle and two handles to one node are not
/// equal in any useful sense.
fn same_node(left: &CppSyntaxNode, right: &CppSyntaxNode) -> bool {
    left.text_range() == right.text_range() && left.kind() == right.kind()
}

/// **Which label is in force at an offset inside a class body**, read from the body's text in order.
fn visibility_of(class: &CppSyntaxNode, offset: usize) -> Access {
    let text = class.text().to_string();
    let start: usize = class.text_range().start().into();
    let Some(up_to_the_member) = text.get(..offset.saturating_sub(start)) else {
        return Access::Public;
    };

    // `class` starts private and `struct`/`union` start public — the language's own defaults, and the reason the
    // keyword is looked at rather than assumed.
    let head = up_to_the_member.trim_start();
    let mut in_force = if head.starts_with("class ") {
        Access::Private
    } else {
        Access::Public
    };

    // The last label written before the member wins. Searched from the front so that a `private:` in a member
    // function's *body* — a label inside a nested class, say — is still read in the order it appears, which is the
    // order the compiler reads them in as well.
    for (at, _) in up_to_the_member.match_indices(':') {
        let before = up_to_the_member[..at].trim_end();
        let word = before
            .rsplit(|character: char| !character.is_alphabetic())
            .next()
            .unwrap_or_default();
        match word {
            // Only a *label* counts: `xpublic: y` is not one, and neither is a word in a string — which this
            // reading cannot tell apart, and which is why the answer is only used to report the two labels that
            // restrict access rather than to report everything that is not `public`.
            "public" => in_force = Access::Public,
            "protected" => in_force = Access::Protected,
            "private" => in_force = Access::Private,
            _ => {}
        }
    }

    in_force
}
