//! **`argument-count`**: a call that passes more arguments than the callee takes, or fewer than it needs.
//!
//! ```cpp
//! int scale(int count, double factor);
//!
//! scale(1, 2.0);        // fine
//! scale(1);             // one argument for two parameters
//! scale(1, 2.0, 3);     // three for two
//! ```
//!
//! # Only a callee this file declares, and that is a fact about the analysis rather than a choice
//!
//! The parameter list of a function declared **here** is in this file's tree, and this check reads it there. For a
//! callee declared elsewhere the honest answer would need that file's tree, which a check is not given — see
//! [`CheckContext`]'s note on why it is handed a model and not a session — so a call into a header is **not
//! judged at all**, however wrong it looks.
//!
//! The field that looks like it would answer this — [`DeclFact::parameters`](crate::DeclFact::parameters) — must
//! not be used: it is the **template** parameter list (its own documentation says "a partial spelling of the
//! template parameters"), and the check this one replaces was measured reading it: on 200 MSVC headers it reported
//! **119 findings**, every one of them a function whose `parameters` was empty and whose real parameters were in
//! the tree all along.
//!
//! # `...`, defaults, and the two directions
//!
//! ```text
//!   too many     only when the callee is not variadic
//!   too few      only below the number of parameters that have no default
//! ```
//!
//! A default is recognised by the `=` the declaration writes, which is the only evidence in the tree — this crate
//! does not evaluate default arguments, so a parameter with one counts as satisfied whether or not the argument
//! could be formed. Erring that way is the direction to err in: a default that cannot be used is the compiler's
//! complaint, and this layer's job is to be quiet when it is not sure.

use cpp_parser::{CppSyntaxKind, CppSyntaxNode};

use super::{Check, CheckContext, DiagnosticCode};

/// **`argument-count`** — see the module documentation.
pub struct ArgumentCount;

/// What a callee declared in this file says about how it may be called.
struct Callee {
    /// The parameters that have no default — a call below this many is missing arguments.
    required: usize,
    /// How many the declaration writes.
    written: usize,
    /// Whether the list ends in `...`.
    variadic: bool,
}

impl Check for ArgumentCount {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ArgumentCount];

    fn check(context: &mut CheckContext<'_>) {
        let callees = callees_in_this_file(context);
        if callees.is_empty() {
            return;
        }

        for call in nodes_of_kind(context, CppSyntaxKind::CallExpr) {
            // The child before the argument list is the callee — `scale` in `scale(1)`, `w.scale` in `w.scale(1)`,
            // and the whole expression in `(make())(1)`. The **last name written** is what a declaration can be
            // found by, which is what `spelling_of` answers.
            let Some(callee) = call.children().next() else {
                continue;
            };
            let Some(name) = spelling_of(&callee) else {
                continue;
            };
            let Some(declared) = callees.get(name.as_str()) else {
                // Declared in another file: not judged — see the module documentation.
                continue;
            };

            let Some(arguments) = call
                .children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ArgumentList)
            else {
                continue;
            };
            let passed = arguments.children().count();

            if !declared.variadic && passed > declared.written {
                context.add(
                    DiagnosticCode::ArgumentCount,
                    cpp_parser::source_range(call.text_range()),
                    name.clone(),
                    at_most(&name, declared.written),
                );
            } else if passed < declared.required {
                context.add(
                    DiagnosticCode::ArgumentCount,
                    cpp_parser::source_range(call.text_range()),
                    name.clone(),
                    at_least(&name, declared.required),
                );
            }
        }
    }
}

/// **Every function this file declares, by the name a call would write.** See [`Callee`].
///
/// Built once per check run: a file with a hundred calls and one declaration should read the declaration once, and
/// the map is what makes the cost the calls rather than the calls times the declarations.
fn callees_in_this_file(context: &CheckContext<'_>) -> std::collections::HashMap<String, Callee> {
    let mut callees = std::collections::HashMap::new();

    for node in nodes_of_kind(context, CppSyntaxKind::FunctionDef)
        .into_iter()
        .chain(nodes_of_kind(context, CppSyntaxKind::Declaration))
    {
        let Some(parameters) = node
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList)
        else {
            continue;
        };
        let Some(name) = name_declared_by(&node) else {
            continue;
        };

        let mut written = 0usize;
        let mut required = 0usize;
        let mut variadic = false;
        for parameter in parameters.children() {
            let text = parameter.text().to_string();
            let text = text.trim();
            if text == "..." || text.ends_with("...") {
                variadic = true;
                continue;
            }
            written += 1;
            // **A default is the `=` the declaration writes** — see the module documentation for why nothing more
            // is asked of it.
            if !text.contains('=') {
                required += 1;
            }
        }

        callees.insert(name, Callee {
            required,
            written,
            variadic,
        });
    }

    callees
}

/// The name a call writes for this declaration — the last identifier in the head, so a return type does not win.
fn name_declared_by(node: &CppSyntaxNode) -> Option<String> {
    let head = node
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList)?;
    let before: Vec<CppSyntaxNode> = node
        .children()
        .take_while(|child| child != &head)
        .collect();
    spelling_of(&before.last()?.clone())
}

/// The last identifier written inside a node, which is the name a call would spell.
fn spelling_of(node: &CppSyntaxNode) -> Option<String> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == cpp_parser::CppTokenKind::Identifier.into())
        .last()
        .map(|token| token.text().to_string())
}

/// Every node of one kind, in one walk — the shape the sibling check's documentation gives a measurement for.
fn nodes_of_kind(context: &CheckContext<'_>, kind: CppSyntaxKind) -> Vec<CppSyntaxNode> {
    context
        .tree()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .collect()
}

fn at_most(name: &str, written: usize) -> String {
    format!("`{name}` takes {written} argument(s) and this call passes more")
}

fn at_least(name: &str, required: usize) -> String {
    format!("`{name}` needs {required} argument(s) and this call passes fewer")
}
