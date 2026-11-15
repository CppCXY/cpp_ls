//! **What a concept requires of its parameter** — the members a constrained template parameter has.
//!
//! ```cpp
//! template <class T> concept A = requires(T a) { { a.foo() } -> B; };
//!
//! auto f(A auto a) {
//!     a.       // <- `foo`, and nothing else is knowable here
//! }
//! ```
//!
//! `a` is a **template parameter** constrained by `A`: there is no class whose members it has, and no type any lookup
//! can find. What says what `a` can do is the concept's own `requires` body — `{ a.foo() }` — and every type that
//! satisfies `A` must provide exactly those names. So completion after `a.` lists them, which is the answer the
//! language gives at that point.
//!
//! Measured on `cpp_modules/src/test.cpp`: `a.` inside `auto f(A auto a)` offered **nothing** before this.
//!
//! # What it does not do, and why
//!
//! Only the names the concept's requires-expression writes on its **own parameter**. A requirement written through
//! another concept (`-> B`) constrains the *result*, not a name of `a`, and following it into `B` would be claiming
//! members of the returned type. A name reached through a call on the parameter (`a.foo().bar`) is not listed either:
//! `bar` is a member of whatever `foo` returns, which the concept has not said.

use super::*;

impl<F: FileProvider + Clone> Session<F> {
    /// **The completion items the concept constraining a parameter requires** — one per the names the concept names
    /// on that parameter, with the concept shown in the detail line.
    ///
    /// `at` is a **reading** offset (the tree's coordinates) and `replace` is the edit range the caller's completion
    /// context carries; an empty range is the ordinary `a.` with the cursor right after the dot.
    pub fn the_members_a_concept_requires(
        &self,
        view: &FileView,
        at: usize,
        replace: cpp_parser::SourceRange,
    ) -> Vec<crate::CompletionItem> {
        let Some(access) = crate::sema::resolve::member_access_at(&view.root, at) else {
            return Vec::new();
        };
        // Only the empty member: `a.` is the question, `a.foo` is a name the reader already wrote.
        if !access.member.is_empty() {
            return Vec::new();
        }
        let object_at = usize::from(access.object.text_range().start());
        let Known::Yes(binding) =
            crate::sema::resolve::definition_at(&view.scopes, &view.root, object_at)
        else {
            return Vec::new();
        };
        let Some(concept) = the_concept_constraining(&view.root, binding.name_range.start_offset)
        else {
            return Vec::new();
        };
        let Some(required) = self.the_members_the_concept_names(&concept, &view.path) else {
            return Vec::new();
        };
        required
            .into_iter()
            .map(|(name, called)| {
                let mut item = crate::CompletionItem::word(
                    &name,
                    if called {
                        crate::completion::ItemKind::Method
                    } else {
                        crate::completion::ItemKind::Field
                    },
                    &format!("required by concept `{concept}`"),
                );
                item.replace = replace;
                item
            })
            .collect()
    }

    /// The member names a concept's `requires` expressions write on its parameter, as `(name, called)`.
    ///
    /// `called` is the difference between `{ a.foo() }` and `a.bar` — a method and a field, which a client draws
    /// differently and which a reader may write differently.
    fn the_members_the_concept_names(
        &self,
        concept: &str,
        path: &Path,
    ) -> Option<Vec<(String, bool)>> {
        let found = self.store.index().definition(concept, path).value()?;
        let view = self.view_of_the_file(&found.file)?;
        let token = cpp_parser::token_at(&view.root, found.fact.name_range.start_offset)?;
        let concept_node = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ConceptDecl
        })?;

        let mut names: Vec<(String, bool)> = Vec::new();
        for requires in concept_node.descendants().filter(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::RequiresExpr
        }) {
            let parameters = the_parameter_names_of(&requires);
            if parameters.is_empty() {
                continue;
            }
            for node in requires.descendants() {
                let kind = cpp_parser::CppSyntaxKind::from(node.kind());
                if !matches!(
                    kind,
                    cpp_parser::CppSyntaxKind::IndexExpr | cpp_parser::CppSyntaxKind::ArrowExpr
                ) {
                    continue;
                }
                let Some(access) = crate::sema::resolve::member_access_of(&node) else {
                    continue;
                };
                if access.member.is_empty() {
                    continue;
                }
                let Some(object) = node.children().next() else {
                    continue;
                };
                let object = object.text().to_string();
                let object = object.trim();
                if !parameters.iter().any(|parameter| parameter == object) {
                    continue;
                }
                let called = node.parent().is_some_and(|parent| {
                    cpp_parser::CppSyntaxKind::from(parent.kind())
                        == cpp_parser::CppSyntaxKind::CallExpr
                });
                if !names.iter().any(|(name, _)| name == &access.member) {
                    names.push((access.member.clone(), called));
                }
            }
        }
        Some(names)
    }
}

/// **The concept that constrains the parameter whose name is at `name`** — `A` for `A auto a`.
///
/// The parameter list is a `ParameterList` node whose contents are tokens, so the constraint is read from the text
/// between the previous comma and the name: the word before `auto` is the concept. A parameter written `T a` under
/// `template <A T>` names a template parameter instead, and its constraint is in the head the declaration sits under —
/// also read from the text, one level up.
fn the_concept_constraining(root: &cpp_parser::CppSyntaxNode, name: usize) -> Option<String> {
    let token = cpp_parser::token_at(root, name)?;
    let list = token.parent_ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ParameterList
    })?;
    let text = list.text().to_string();
    let start = usize::from(list.text_range().start());
    let before = text.get(..name.saturating_sub(start))?;
    let parameter = before.rsplit(',').next()?.trim();
    let words = the_words(parameter);
    if let Some(at) = words.iter().position(|word| *word == "auto") {
        return words
            .get(at.checked_sub(1)?)
            .map(|word| (*word).to_string());
    }
    // **`template <A T> … (T a)`** — the type names a template parameter, so the constraint is on the head. The word
    // before the parameter's own name is its constraint, unless it is the keyword that introduces it.
    let written_type = words.last()?;
    let declaration = list.ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Declaration
    })?;
    // **The head is a *child* of the declaration the function sits in**, not an ancestor of the parameter list: the
    // grammar keeps `template <...>` and what it introduces in one `Declaration`, so the list is found one level down
    // from an ancestor of the parameter.
    // The head sits under a TemplateDecl inside that declaration, not directly under it — the grammar keeps
    // template <...> and what it introduces apart so that a caller can tell a template from its declaration.
    let head = declaration.ancestors().find_map(|ancestor| {
        let head = ancestor.children().find(|child| {
            cpp_parser::CppSyntaxKind::from(child.kind())
                == cpp_parser::CppSyntaxKind::TemplateParameterList
        });
        head.or_else(|| {
            ancestor
                .children()
                .find(|child| {
                    cpp_parser::CppSyntaxKind::from(child.kind())
                        == cpp_parser::CppSyntaxKind::TemplateDecl
                })
                .and_then(|introduced| {
                    introduced.children().find(|child| {
                        cpp_parser::CppSyntaxKind::from(child.kind())
                            == cpp_parser::CppSyntaxKind::TemplateParameterList
                    })
                })
        })
    })?;
    // **The head's parameters may be nodes or plain tokens** — same shape as a `ParameterList`, so the text is the
    // one reader that answers both: `(HasFoo T)` split at the commas names each parameter, and the word before the
    // one being looked for is its constraint.
    let mut heads: Vec<String> = head
        .children()
        .map(|parameter| parameter.text().to_string())
        .collect();
    if heads.is_empty() {
        let text = head.text().to_string();
        let inside = text.trim_start_matches('<').trim_end_matches('>');
        heads = inside.split(',').map(|part| part.to_string()).collect();
    }
    for parameter in heads {
        let words = the_words(&parameter);
        let Some(last) = words.last() else {
            continue;
        };
        if last != written_type || words.len() < 2 {
            continue;
        }
        let constraint = words[words.len() - 2];
        if matches!(constraint, "class" | "typename" | "template" | "concept") {
            continue;
        }
        return Some(constraint.to_string());
    }
    None
}

/// The identifier-shaped words of a spelling, in order.
fn the_words(text: &str) -> Vec<&str> {
    text.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .collect()
}

/// The names a requires-expression binds — `(T a)` is `a`, `(T a, U b)` is `a` and `b`.
fn the_parameter_names_of(requires: &cpp_parser::CppSyntaxNode) -> Vec<String> {
    let Some(open) = requires.text().to_string().find('(') else {
        return Vec::new();
    };
    let Some(close) = requires.text().to_string()[open..].find(')') else {
        return Vec::new();
    };
    let inside = &requires.text().to_string()[open + 1..open + close];
    inside
        .split(',')
        .filter_map(|parameter| {
            parameter
                .rsplit(|character: char| !character.is_alphanumeric() && character != '_')
                .find(|word| !word.is_empty())
                .map(|word| word.to_string())
        })
        .collect()
}
