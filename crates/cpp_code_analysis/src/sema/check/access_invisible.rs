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
        // **Every member access in this file's own text**, by the sema layer's definition — see
        // `CheckContext::member_accesses` for why the node kind cannot be trusted for this question, and why this
        // check found nothing at all until it stopped filtering on `MemberExpr`.
        for (node, access) in context.member_accesses() {
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
            // is the name's. A member declared in another file is not here, and this check does not judge it, so the
            // search runs over this file's own nodes rather than over the whole rendering.
            let Some(declaration) = context
                .own_syntax_nodes()
                .find(|node| usize::from(node.text_range().start()) == member.fact.range.start_offset)
            else {
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



/// The nearest ancestor that is a **class definition** — `class`, `struct`, `union`.
///
/// Asked of the tree, which has a node for it: `Declaration > DeclSpecifierSeq > ClassDef`, with a `ClassKeyword`
/// or `StructKeyword` token inside. This was a text test — *does the text start with `class ` and hold a `{`* — and
/// that is a decision made by reading punctuation, which is how the check layer produced its one false positive so
/// far (see `assignment_in_condition`).
fn enclosing_definition(node: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if super::is_a_class_definition(&ancestor) {
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

/// **Which label is in force at an offset inside a class definition.**
///
/// Two facts, both from the tree:
///
/// ```text
///   the default    the `ClassKeyword`/`StructKeyword` token of the definition: `class` starts private,
///                  `struct` and `union` start public
///   the last label the last `PublicAccess`/`ProtectedAccess`/`PrivateAccess` node written **before** the member,
///                  in the order the class body holds its children
/// ```
///
/// This was a scan of the body's text for `:` and the word before it, which is a decision made by reading
/// punctuation — and the one the check layer's own rule forbids, for the reason the rule exists: a `:` inside a
/// string literal in a default argument is not a label, and text cannot tell.
fn visibility_of(class: &CppSyntaxNode, offset: usize) -> Access {
    let mut in_force = default_access_of(class);

    let Some(body) = class
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ClassBody)
    else {
        return in_force;
    };

    for child in body.children() {
        // Nothing after the member is in force *at* the member.
        if usize::from(child.text_range().start()) >= offset {
            break;
        }
        in_force = match CppSyntaxKind::from(child.kind()) {
            CppSyntaxKind::PublicAccess => Access::Public,
            CppSyntaxKind::ProtectedAccess => Access::Protected,
            CppSyntaxKind::PrivateAccess => Access::Private,
            // A member or anything else: the label in force is unchanged.
            _ => in_force,
        };
    }

    in_force
}

/// **The access a class definition starts with**, read from its own keyword.
///
/// `class` is private and `struct`/`union` are public — the language's rule, and the reason the keyword is a token
/// this asks for rather than a word compared against a header's spelling.
fn default_access_of(class: &CppSyntaxNode) -> Access {
    let keyword = class
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| {
            // **One token's word, not a scan of the class's text.** The boundary is the lexer's - this is the first
            // token of the definition - and the comparison is against one of the three words the language gives that
            // position. Comparing the token's own text is what a token is for; the difference from the scan this
            // replaced is that a `class` inside a string or a comment is not a token, so it cannot be found here.
            matches!(token.text().to_string().as_str(), "class" | "struct" | "union")
        });

    match keyword.map(|token| token.text().to_string()) {
        Some(word) if word == "class" => Access::Private,
        _ => Access::Public,
    }
}







