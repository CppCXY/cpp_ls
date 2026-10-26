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

use cpp_parser::CppSyntaxNode;

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
        // **Only a name this file declares can be judged.** Both sides of this rule have to be in the file being
        // checked (see the module documentation), so a member whose name no fact of this file records cannot produce
        // a finding — and finding out that it is not there costs an inference per access. Measured on
        // `LuaSyntaxTree.cpp`: the rule is the most expensive one at ~230 ms, and almost every access it looked at
        // was `tokens.size()`, `e.U.Start.Kind` — members of classes from other files, which this rule never judges.
        let declared_here: std::collections::HashSet<&str> = context
            .summary
            .declarations
            .iter()
            .map(|fact| fact.name.as_str())
            .collect();

        // **Every member access in this file's own text**, by the sema layer's definition — see
        // `CheckContext::member_accesses` for why the node kind cannot be trusted for this question, and why this
        // check found nothing at all until it stopped filtering on `MemberExpr`.
        for (node, access) in context.member_accesses() {
            if access.member.is_empty() || !declared_here.contains(access.member.as_str()) {
                continue;
            }

            let crate::Known::Yes((object, _)) = context.type_of(&access.object) else {
                continue;
            };
            let crate::Known::Yes(members) = context
                .model
                .members_of(&object.class_name().unwrap_or_default())
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
            // node must also be one this file wrote (see `own_syntax_nodes`).
            //
            // **Not `own_syntax_nodes().find(..)`**, which is what this was: that iterator walks the whole file's
            // tree, and it did so **once per member access**. Measured on `LuaSyntaxTree.cpp` — a 2 800-line file
            // with several hundred accesses — the rule cost **234 ms**, and the walk is hidden behind the type gate:
            // `CPPLS_NO_INFERENCE=1` makes the rule answer before it ever gets here, so the A/B that attributed
            // 96% of the layer to inference did not see it. A token at the offset and its ancestors reach the same
            // node in O(depth).
            let Some(declaration) =
                declaration_starting_at(context, member.fact.range.start_offset)
            else {
                continue;
            };
            if !context
                .view
                .was_written_here(cpp_parser::source_range(declaration.text_range()))
            {
                continue;
            }
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

/// **The outermost node of the tree that starts at `offset`** — the declaration a member fact's range names.
///
/// The token at that offset and its ancestors are the only nodes that can start there, so this is O(depth) where
/// [`super::CheckContext::own_syntax_nodes`] is O(the file). Ancestors come innermost first, and the old walk
/// answered with the first node in *pre-order* — the outermost — so the last match is the one to keep.
fn declaration_starting_at(
    context: &super::CheckContext<'_>,
    offset: usize,
) -> Option<CppSyntaxNode> {
    let token = cpp_parser::token_at(context.tree(), offset)?;
    token
        .parent_ancestors()
        .filter(|node| usize::from(node.text_range().start()) == offset)
        .last()
}

/// Whether two handles are the same node — by range, because a node is a handle and two handles to one node are not
/// equal in any useful sense.
fn same_node(left: &CppSyntaxNode, right: &CppSyntaxNode) -> bool {
    left.text_range() == right.text_range() && left.kind() == right.kind()
}

/// **Which label is in force at an offset inside a class definition.**
///
/// Two facts, both from the typed layer:
///
/// ```text
///   the default    the definition's own keyword token: `class` starts private, `struct`/`union` start public
///   the last label the last `CppAccessSpecifier` node written **before** the member, in the order the class body
///                  holds its children — read through `get_keyword`, so the three labels are one type
/// ```
///
/// This was a scan of the body's text for `:` and the word before it, which is a decision made by reading
/// punctuation — and the one the check layer's own rule forbids, for the reason the rule exists: a `:` inside a
/// string literal in a default argument is not a label, and text cannot tell.
fn visibility_of(class: &CppSyntaxNode, offset: usize) -> Access {
    use cpp_parser::{CppAccessSpecifier, CppAstNode, CppAstToken, CppClassBody};

    let mut in_force = default_access_of(class);

    let Some(body) = cpp_parser::CppClassDef::cast(class.clone())
        .and_then(|definition| definition.children::<CppClassBody>().next())
    else {
        return in_force;
    };

    for child in body.syntax().children() {
        // Nothing after the member is in force *at* the member.
        if usize::from(child.text_range().start()) >= offset {
            break;
        }
        let Some(specifier) = CppAccessSpecifier::cast(child) else {
            // A member or anything else: the label in force is unchanged.
            continue;
        };
        in_force = match specifier.get_keyword().map(|keyword| {
            cpp_parser::CppTokenKind::from(cpp_parser::CppKind::from(keyword.syntax().kind()))
        }) {
            Some(cpp_parser::CppTokenKind::PublicKeyword) => Access::Public,
            Some(cpp_parser::CppTokenKind::ProtectedKeyword) => Access::Protected,
            Some(cpp_parser::CppTokenKind::PrivateKeyword) => Access::Private,
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
    use cpp_parser::{CppAstNode, CppClassDef, CppKind, CppTokenKind};

    // **By token kind, not by the token's word.** This crate's rowan kind is `CppKind` — the wrapper around
    // `Node(CppSyntaxKind)` and `Token(CppTokenKind)` — so a token's kind has to be unwrapped before it means
    // anything, and `examples/token_kinds.rs` prints both halves for any source when that is in doubt.
    let keyword = CppClassDef::cast(class.clone()).and_then(|definition| {
        definition
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| {
                matches!(
                    CppTokenKind::from(CppKind::from(token.kind())),
                    CppTokenKind::ClassKeyword
                        | CppTokenKind::StructKeyword
                        | CppTokenKind::UnionKeyword
                )
            })
    });

    match keyword.map(|token| CppTokenKind::from(CppKind::from(token.kind()))) {
        Some(CppTokenKind::ClassKeyword) => Access::Private,
        _ => Access::Public,
    }
}
