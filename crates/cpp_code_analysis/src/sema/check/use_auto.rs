//! `modernize-use-auto`, from clang-tidy, narrowed to the case that is never wrong: **an iterator declared by its full
//! spelling.**
//!
//! ```cpp
//! for (std::vector<Widget>::iterator it = widgets.begin(); …)   // this check says: auto it = …
//! ```
//!
//! The full spelling is the worst kind of maintenance: rename the container's element type and every declaration of its
//! iterator has to change with it, and the compiler says nothing until the one place that was missed. `auto` is the same
//! type, by the language's own deduction.
//!
//! # How it is found
//!
//! A scan of the text for `::iterator` and `::const_iterator` followed by an identifier — the two spellings every standard
//! container defines, and the only ones this rule claims. The general `modernize-use-auto` (any declaration whose type is
//! written twice) needs the type to be *known*, which the summary does not carry; this narrowed rule needs only the text.

use super::{Check, CheckContext, DiagnosticCode};

pub struct UseAuto;

impl Check for UseAuto {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UseAuto];

    fn check(context: &mut CheckContext<'_>) {
        // **The declarations, not a scan of the file.** The rule is about a declaration whose *type* is written with its
        // full iterator spelling, and the parser has a node for the type: the declaration's specifier sequence. Reading
        // that node's own text asks the question exactly — it cannot match a comment, a string, or a `::iterator` in a
        // template argument that declares nothing, all of which the previous text scan had to defend against by hand.
        use cpp_parser::CppAstNode;
        for declaration in context
            .own::<cpp_parser::CppDeclaration>()
            .collect::<Vec<_>>()
        {
            let Some(specifiers) = declaration.child::<cpp_parser::CppDeclSpecifierSeq>() else {
                continue;
            };
            let written = context.node_text(specifiers.syntax()).trim_end();
            let spelling = if written.ends_with("::const_iterator") {
                "::const_iterator"
            } else if written.ends_with("::iterator") {
                "::iterator"
            } else {
                continue;
            };
            // **A declaration, not a mention.** At least one declarator has to carry a plain name, which is what
            // `std::vector<int>::iterator it` does and what a parameter of that type (`void f(std::vector<int>::iterator)`)
            // does not — the parameter declares no iterator the author could rewrite as `auto`.
            let mut named: Option<(String, cpp_parser::SourceRange)> = None;
            for declarator in declaration.children::<cpp_parser::CppInitDeclarator>() {
                let Some(name) = declarator.child::<cpp_parser::CppDeclarator>() else {
                    continue;
                };
                // **A plain declarator** — `it`, not `it[4]` and not `(*it)`: its whole text is one identifier, which is the
                // declaration `auto` can replace.
                let text = context.node_text(name.syntax()).trim();
                let is_one_name = !text.is_empty()
                    && text
                        .chars()
                        .all(|character| character.is_alphanumeric() || character == '_');
                if is_one_name {
                    named = Some((
                        text.to_string(),
                        cpp_parser::source_range(name.syntax().text_range()),
                    ));
                    break;
                }
            }
            let Some((name, range)) = named else {
                continue;
            };
            context.add(
                DiagnosticCode::UseAuto,
                cpp_parser::source_range(specifiers.syntax().text_range()),
                t!("%{spelling} %{name}", spelling = spelling, name = name),
                t!(
                    "`%{name}` is declared by its full iterator spelling — write `auto %{name}`, which is the same type and \
                     cannot disagree with the initialiser"
                ),
            );
            let _ = range;
        }
    }
}
