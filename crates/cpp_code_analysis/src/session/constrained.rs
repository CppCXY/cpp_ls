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
        // **The object may be a call whose result a concept constrains** — `{ a.foo() } -> B` says the value `foo`
        // returns satisfies `B`, so `a.foo().` has the members `B` requires, and `a.foo().get().` the ones `B`'s own
        // result constraint requires. The chain is walked from the inside out, one call at a time, so a program that
        // states its types in requirements rather than in classes gets as many hops as it wrote.
        let concepts = self.the_concepts_of_an_expression(view, &access.object, 0);
        let mut items: Vec<crate::CompletionItem> = Vec::new();
        for concept in &concepts {
            let Some(required) = self.the_members_the_concept_names(concept, &view.path) else {
                continue;
            };
            for (name, called) in required {
                // **One row per name**, even when two concepts require it: the list is something a reader picks from,
                // and the detail line names the concept that was asked first.
                if items.iter().any(|item| item.label == name) {
                    continue;
                }
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
                items.push(item);
            }
        }
        items
    }

    /// **The concepts that constrain what an expression's value satisfies** — the walk a chain of required calls
    /// needs.
    ///
    /// A name answers with the concepts its declaration is constrained by (`A auto a`); a call answers with the
    /// concepts its *receiver's* concepts say the call's result satisfies (`{ a.foo() } -> B`), which is the only
    /// statement of that result's members anywhere in the program. Recursion is on the receiver, so
    /// `a.foo().get()` is `get` applied to what `B` says `foo` returns, and the depth is bounded because a program
    /// may state the chain in a circle.
    fn the_concepts_of_an_expression(
        &self,
        view: &FileView,
        expression: &cpp_parser::CppSyntaxNode,
        depth: usize,
    ) -> Vec<String> {
        if depth > 8 {
            return Vec::new();
        }
        if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::CallExpr
            && let Some(member) = the_member_a_call_names(expression)
            && let Some(callee) = expression.children().next()
            && let Some(receiver) = callee.children().next()
        {
            let mut found: Vec<String> = Vec::new();
            for concept in self.the_concepts_of_an_expression(view, &receiver, depth + 1) {
                for result in self.the_result_concepts_of(&concept, &view.path, &member) {
                    if !found.contains(&result) {
                        found.push(result);
                    }
                }
            }
            return found;
        }
        let at = usize::from(expression.text_range().start());
        let Known::Yes(binding) = crate::sema::resolve::definition_at(&view.scopes, &view.root, at)
        else {
            return Vec::new();
        };
        the_concepts_constraining(&view.root, binding.name_range.start_offset)
    }

    /// **The concepts a requirement's result is constrained by** — `B` for `{ a.foo() } -> B`, when the call is a
    /// `foo`.
    ///
    /// A concept's requires-body can state what a *call* returns (`{ x.make() } -> Widget`), and that constraint is
    /// the only statement of the result's members anywhere in the program. The requirement is matched by the member
    /// the call names — the concept's parameter is its own (`{ t.foo() }`), so the expression's text cannot be
    /// compared with the reader's.
    fn the_result_concepts_of(&self, concept: &str, path: &Path, member: &str) -> Vec<String> {
        let Some(found) = self.store.index().definition(concept, path).value() else {
            return Vec::new();
        };
        let Some(view) = self.view_of_the_file(&found.file) else {
            return Vec::new();
        };
        let Some(token) = cpp_parser::token_at(&view.root, found.fact.name_range.start_offset)
        else {
            return Vec::new();
        };
        let Some(concept_node) = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ConceptDecl
        }) else {
            return Vec::new();
        };
        let mut concepts: Vec<String> = Vec::new();
        for requirement in concept_node.descendants().filter(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Requirement
        }) {
            let text = requirement.text().to_string();
            let Some(arrow) = text.find("->") else {
                continue;
            };
            let expression = text[..arrow].trim();
            // `{ a.foo() }` names the member; `a.foo() == 42` does not constrain its result and is not matched.
            if !expression.contains(&format!(".{member}("))
                && !expression.contains(&format!("->{member}("))
            {
                continue;
            }
            let constraint = text[arrow + 2..].trim().trim_end_matches(';').trim();
            for concept in the_concepts_a_constraint_names(constraint) {
                if !concepts.contains(&concept) {
                    concepts.push(concept);
                }
            }
        }
        concepts
    }

    /// The member names a concept's `requires` expressions write on its parameter, as `(name, called)`.
    ///
    /// `called` is the difference between `{ a.foo() }` and `a.bar` — a method and a field, which a client draws
    /// differently and which a reader may write differently.
    ///
    /// **A concept whose body is another concept is followed**, because that is how a library states one constraint in
    /// several: `concept Combinable = HasFoo<T> && HasBar<T>;` requires both sets of names, and answering only the
    /// first would be a popup that lists half of what the type must provide. The walk is bounded (a chain longer than
    /// eight, or a cycle, stops) and visits each concept once.
    fn the_members_the_concept_names(
        &self,
        concept: &str,
        path: &Path,
    ) -> Option<Vec<(String, bool)>> {
        let mut names: Vec<(String, bool)> = Vec::new();
        let mut visited: Vec<String> = Vec::new();
        self.collect_the_members_of_a_concept(concept, path, 0, &mut visited, &mut names);
        Some(names)
    }

    fn collect_the_members_of_a_concept(
        &self,
        concept: &str,
        path: &Path,
        depth: usize,
        visited: &mut Vec<String>,
        names: &mut Vec<(String, bool)>,
    ) {
        if depth > 8 || visited.iter().any(|seen| seen == concept) {
            return;
        }
        visited.push(concept.to_string());
        let Some(found) = self.store.index().definition(concept, path).value() else {
            return;
        };
        let Some(view) = self.view_of_the_file(&found.file) else {
            return;
        };
        let Some(token) = cpp_parser::token_at(&view.root, found.fact.name_range.start_offset)
        else {
            return;
        };
        let Some(concept_node) = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ConceptDecl
        }) else {
            return;
        };

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

        for other in the_concepts_a_body_names(&concept_node) {
            self.collect_the_members_of_a_concept(&other, &found.file, depth + 1, visited, names);
        }
    }
}

/// **The concepts that constrain the parameter whose name is at `name`** — `A` for `A auto a`.
///
/// Three spellings say it, and a parameter may be constrained by more than one of them:
///
/// * `A auto a` — the word before `auto`, read from the text between the previous comma and the name (a
///   `ParameterList`'s contents are tokens, not nodes);
/// * `template <A T> ... (T t)` — the parameter's type names a template parameter, and the constraint is in the head
///   one level up, under a `TemplateDecl`;
/// * `template <class T> requires A<T> ...` — a requires-clause on the declaration, whose terms name concepts.
fn the_concepts_constraining(root: &cpp_parser::CppSyntaxNode, name: usize) -> Vec<String> {
    let mut concepts: Vec<String> = Vec::new();
    let Some(token) = cpp_parser::token_at(root, name) else {
        return concepts;
    };
    let Some(list) = token.parent_ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ParameterList
    }) else {
        return concepts;
    };
    let text = list.text().to_string();
    let start = usize::from(list.text_range().start());
    let Some(before) = text.get(..name.saturating_sub(start)) else {
        return concepts;
    };
    let parameter = before.rsplit(',').next().unwrap_or_default().trim();
    let words = the_words(parameter);
    if let Some(at) = words.iter().position(|word| *word == "auto") {
        if let Some(constraint) = at.checked_sub(1).and_then(|at| words.get(at)) {
            concepts.push((*constraint).to_string());
        }
    } else if let Some(written_type) = words.last()
        && let Some(constraint) = the_head_constraint_of(&list, written_type)
    {
        concepts.push(constraint);
    }
    // **A requires-clause is a constraint of the same kind**, and it is written beside the template head rather than
    // in it. Read from the text for the same reason as the two above: the clause is a `RequiresClause` node, and the
    // concepts it names are its leading identifiers.
    // **Inside the declaration the parameter belongs to**, not anywhere above it: an ancestor's descendants reach
    // every other declaration in the file, and a clause three functions further down is not this parameter's. Measured
    // the first way: a completion in an unconstrained function offered the names of a concept another function wrote.
    let declaration = list.ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Declaration
    });
    if let Some(clause) = declaration.and_then(|declaration| {
        declaration.descendants().find(|child| {
            cpp_parser::CppSyntaxKind::from(child.kind())
                == cpp_parser::CppSyntaxKind::RequiresClause
        })
    }) {
        for concept in the_concepts_a_constraint_names(&clause.text().to_string()) {
            if !concepts.contains(&concept) {
                concepts.push(concept);
            }
        }
    }
    concepts
}

/// **The constraint written in a template head** — `A` for `template <A T>` when the type being looked for is `T`.
fn the_head_constraint_of(list: &cpp_parser::CppSyntaxNode, written_type: &str) -> Option<String> {
    // **The head is a *child* of the declaration the function sits in**, not an ancestor of the parameter list: the
    // grammar keeps `template <...>` and what it introduces in one `Declaration`, with the head under a
    // `TemplateDecl`, so that a caller can tell a template from its declaration.
    let declaration = list.ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Declaration
    })?;
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
        if *last != written_type || words.len() < 2 {
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

/// **The concept names a constraint expression applies** — `A<T> && B<T>` names two, `sizeof(T) > 0` names none.
fn the_concepts_a_constraint_names(text: &str) -> Vec<String> {
    let body = text.trim().trim_start_matches("requires").trim();
    let mut names: Vec<String> = Vec::new();
    for term in body.split("&&").flat_map(|part| part.split("||")) {
        let term = term.trim_matches(|character: char| {
            character == '(' || character == ')' || character.is_whitespace()
        });
        let name: String = term
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect();
        if !name.is_empty()
            && !matches!(name.as_str(), "true" | "false" | "requires")
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// **The member a call names** — `foo` for `a.foo()`.
fn the_member_a_call_names(call: &cpp_parser::CppSyntaxNode) -> Option<String> {
    let callee = call.children().next()?;
    let access = crate::sema::resolve::member_access_of(&callee)?;
    (!access.member.is_empty()).then_some(access.member.clone())
}

/// The identifier-shaped words of a spelling, in order.
fn the_words(text: &str) -> Vec<&str> {
    text.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .collect()
}

/// **The concepts a concept's body applies** — `Combinable`'s `HasFoo<T> && HasBar<T>` names two.
///
/// The body is the text after the `=` that defines the concept. A body that starts with `requires` is a
/// requires-expression, whose member names are collected directly and which names no further concept through `&&`;
/// anything else is a constraint expression, and each of its `&&`/`||` terms starts with the concept it applies.
/// The `=` is searched for **after** the word `concept`, so a default argument in the template head
/// (`template <class T = int>`) is not mistaken for the definition.
fn the_concepts_a_body_names(node: &cpp_parser::CppSyntaxNode) -> Vec<String> {
    let text = node.text().to_string();
    let after_the_keyword = text.find("concept").map_or(0, |at| at + "concept".len());
    let Some(equals) = text[after_the_keyword..].find('=') else {
        return Vec::new();
    };
    let body = text[after_the_keyword + equals + 1..].trim();
    let body = body.trim_end_matches(';').trim();
    if body.starts_with("requires") {
        return Vec::new();
    }
    body.split("&&")
        .flat_map(|part| part.split("||"))
        .filter_map(|term| {
            let term = term.trim_matches(|character: char| {
                character == '(' || character == ')' || character.is_whitespace()
            });
            let name: String = term
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            (!name.is_empty() && !matches!(name.as_str(), "true" | "false" | "requires"))
                .then_some(name)
        })
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
