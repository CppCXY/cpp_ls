//! **The text of a missing member definition** — what a "create the definition" edit writes.
//!
//! ```cpp
//! struct Shape {
//!     virtual double area() const;      // no definition anywhere
//!     void noop();
//! };
//! ```
//!
//! A cursor on `area` asks for `double Shape::area() const { }`, and this module is the answer: the pieces of that
//! line, taken from the declaration the file wrote rather than re-derived from the fact's spelling — [`DeclFact`]
//! holds a return type and no parameter list, and a definition has to repeat the parameters *as written*, names
//! included.
//!
//! # What is answered, and when
//!
//! ```text
//! a member function that is not defined anywhere   -> the pieces of its definition
//! one that is defined, or is pure                  -> nothing (the edit would duplicate a definition, or a body the
//!                                                    derived class must supply)
//! a free function, a variable, a class             -> nothing (the request is about a class's missing member)
//! ```
//!
//! "Defined anywhere" is the index's answer: a fact for the same class and name with [`DeclModifiers::DEFINED`]. The
//! scan is over the summaries the session holds, so a project whose `.cpp` has not been read answers "not defined" and
//! offers the edit — the trade every index-backed answer in this crate makes, and the edit is shown to a reader who can
//! see whether the file already has the function.
//!
//! # Why the tree
//!
//! The parameter list has to be copied verbatim: `double area(int w, int h) const` needs `(int w, int h)` and not
//! `(int, int)`, and a default argument (`double area(int w = 1)`) must not be repeated in the definition. The
//! declaration's own [`CppParamList`](cpp_parser::CppParamList) is that text.

use crate::file::coordinates::{FileOffset, ReadingOffset};
use crate::{DeclKind, DeclModifiers, FileProvider, FileView, Session};

/// **A definition a file does not have yet.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingDefinition {
    /// The class the member belongs to, as the declaration spelled its scope: `Shape`, `ns::Shape`.
    pub class: String,
    /// The member's own name: `area`, `~Shape`, `Shape` for a constructor.
    pub name: String,
    /// `Shape::area` — the qualified name a definition is written with.
    pub qualified_name: String,
    /// The return type as written, or `None` for a constructor or a destructor.
    pub returns: Option<String>,
    /// The parameter list **as written**, parentheses included: `(int w, int h)`.
    pub parameters: String,
    /// The qualifiers written after the parameter list: ` const`, ` &`, ` noexcept`. `override`, `final` and a
    /// contract specifier are not part of a definition and are not carried here.
    pub qualifiers: String,
}

impl MissingDefinition {
    /// **The text of the definition**, ready to insert at the top level of a file.
    ///
    /// ```text
    /// double Shape::area() const
    /// {
    /// }
    /// ```
    ///
    /// No trailing newline of its own: the caller owns the file's layout, and the places that insert this add their
    /// own.
    pub fn text(&self) -> String {
        let mut line = String::new();
        // **A constructor writes no return type**, and the space before the name is the return type`s.
        if let Some(returns) = &self.returns
            && !returns.trim().is_empty()
        {
            line.push_str(returns.trim());
            line.push(' ');
        }
        line.push_str(&self.qualified_name);
        line.push_str(&self.parameters);
        line.push_str(&self.qualifiers);
        format!("{line}\n{{\n}}")
    }
}

impl<F: FileProvider + Clone> Session<F> {
    /// **The definition of the member at the cursor, if it has none.**
    pub fn missing_definition_at(
        &self,
        view: &FileView,
        offset: ReadingOffset,
    ) -> Option<MissingDefinition> {
        // **The declaration at the cursor, by its name token** — the fact whose `name_range` starts where the token
        // under the cursor was written. Asked of the summary rather than of `definitions`: the name here is a
        // *declaration*, and the fact is the one the file's own walk produced, modifiers and scope included.
        let in_the_file = view.file_offset_of(offset)?.get();
        let summary = self.index().summary(&view.path)?;
        let fact = summary
            .declarations
            .iter()
            .find(|fact| fact.name_range.start_offset == in_the_file)?;
        if fact.kind != DeclKind::Function
            || fact.modifiers.contains(DeclModifiers::DEFINED)
            || fact.modifiers.contains(DeclModifiers::PURE)
        {
            return None;
        }
        // **A member, not a free function**: the definition of a free function *is* the function, and this edit exists
        // for the declaration/definition split a class makes.
        let class = fact.scope.clone()?;
        let name = fact.name.clone();

        // **Already defined somewhere** — the question is the whole point of the edit, and a second definition is
        // worse than no edit.
        for (_, summary) in self.index().all_summaries() {
            let defined = summary.declarations.iter().any(|other| {
                other.kind == DeclKind::Function
                    && other.name == name
                    && other.scope.as_deref() == Some(class.as_str())
                    && other.modifiers.contains(DeclModifiers::DEFINED)
            });
            if defined {
                return None;
            }
        }

        // **The pieces the fact does not hold**, read from the declaration's own declarator.
        let (parameters, qualifiers) =
            written_parameters_and_qualifiers(view, fact.name_range.start_offset)?;
        Some(MissingDefinition {
            qualified_name: format!("{class}::{name}"),
            class,
            name,
            returns: fact.returns.clone(),
            parameters,
            qualifiers,
        })
    }
}

/// **The parameter list and the trailing qualifiers as written**, from the declaration whose name is at
/// `in_the_file` (a **file** offset, which is what the summary's ranges are).
fn written_parameters_and_qualifiers(
    view: &FileView,
    in_the_file: usize,
) -> Option<(String, String)> {
    use cpp_parser::CppAstNode;
    let reading = view.reading_offset_of(FileOffset::new(in_the_file))?;
    let token = cpp_parser::token_at(&view.root, reading.get())?;
    let declarator = token.parent()?.ancestors().find(|node| {
        cpp_parser::CppDeclarator::can_cast(node.kind().into())
    })?;
    let parameters = declarator
        .children()
        .find_map(cpp_parser::CppParamList::cast)?;
    let text = parameters.syntax().text().to_string();
    let after = parameters.syntax().text_range().end();

    // **The qualifiers written after the parameter list**: `const`, `&`, `&&` and `noexcept`, in the declaration that
    // holds the declarator. `override` and `final` are not part of a definition, a contract specifier is not either,
    // and an operand of `noexcept(…)` is dropped because a definition may write the plain spelling.
    let declaration = declarator.ancestors().find(|node| {
        cpp_parser::CppDeclaration::can_cast(node.kind().into())
    })?;
    let mut words: Vec<String> = Vec::new();
    for element in declaration.descendants_with_tokens() {
        let Some(token) = element.as_token() else {
            continue;
        };
        if token.text_range().start() < after {
            continue;
        }
        let word = token.text();
        match word {
            "const" => words.push("const".to_string()),
            "&" | "&&" => words.push(word.to_string()),
            "noexcept" if !words.iter().any(|held| held == "noexcept") => {
                words.push("noexcept".to_string());
            }
            // Everything else after the parameter list — `override`, `final`, `->`, a body — is not a qualifier this
            // definition repeats.
            _ => {}
        }
    }
    let qualifiers = if words.is_empty() {
        String::new()
    } else {
        format!(" {}", words.join(" "))
    };
    Some((text, qualifiers))
}
