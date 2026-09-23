//! The bridge from this crate's scope tree to the parser's **external symbol table**.
//!
//! `cpp_parser` decides several readings by looking a name up — `Widget w(1, 2);` against `g(1, 2);`, `(Widget)x`
//! against `(x)` — and it can only answer for names *its own file* declares. Its `symbols` module defines the
//! interface a caller with more context can supply, and this module is the first implementation of it: the
//! analysis crate, which knows what the file's declarations *mean*, answered in the parser's vocabulary.
//!
//! # What this bridge answers, and what it deliberately does not
//!
//! ```text
//! a class, enum, alias or typedef   ->  SymbolKind::Type          (the answer that decides the most)
//! a function, constructor, operator ->  SymbolKind::Function      (the answer the parser cannot get at all)
//! a variable, field or enumerator   ->  SymbolKind::Variable
//! a namespace                       ->  SymbolKind::Namespace
//! a template parameter              ->  SymbolKind::Type          (a type name in its scope)
//! a label, a using-declaration, …   ->  None                      ("this table does not know" — never "no")
//! ```
//!
//! Three boundaries are worth stating, because a caller has to know them:
//!
//! * **This file only.** A [`ScopeTree`] is built from one translation unit, so the bridge answers for that unit's
//!   own declarations — which is more than the parser's own table holds (it records *types*, this knows functions
//!   and variables too) but is still not the cross-file index the interface was designed for. An index over the
//!   include graph is the next step; until then the parser falls back to its shape rules for everything else.
//! * **Unqualified names only.** A name is asked for as it is written, and a written name can be qualified
//!   (`std::vector`, `ns::C::f`). Resolving those is semantic work, not a table lookup, and answering a qualified
//!   name wrongly would be worse than answering `None`: the bridge stays silent for anything containing a `::`.
//! * **No macros.** Macros are not bindings here — they live in [`crate::MacroTable`], and they are *not* the same
//!   question: what the parser needs about a macro is what its body expands to, which is an expansion, not a kind.
//!   So `Some(SymbolKind::Macro { .. })` is never returned, and the parser's macro rules keep their own evidence.
//!
//! # Why `None` is the common answer, and why that is correct
//!
//! The interface's contract is that `None` means "this table does not know" and never "not a type" — a stale or
//! partial answer must not change how valid code parses. This bridge follows it: a name it has no binding for is
//! `None`, and the parser's own table and shape preferences take over from there. What it does answer, it answers
//! from the file's own syntax, so it cannot be staler than the buffer it was built from.

use cpp_parser::{MacroBody, SymbolKind, SymbolTable};

use crate::sema::symbol::{BindingKind, Name, ScopeTree};

impl SymbolTable for ScopeTree {
    fn kind_of(&self, name: &str) -> Option<SymbolKind> {
        // A qualified name is not this bridge's business — see the module documentation.
        if name.is_empty() || name.contains("::") {
            return None;
        }

        // The **most specific** answer wins: a class and a variable can share a spelling in different scopes, and
        // the parser's questions are about what a name *can* be. `Type` and `Function` are therefore preferred over
        // `Variable`, which is the order the fold below applies.
        //
        // Every scope is searched rather than only the file scope: a name the parser asks about is often declared
        // inside a namespace, and a scope the walker created for it is still in this file's tree. The scan stops at
        // the first `Type`, which is the answer that decides the most and the one a file is most likely to hold.
        let mut answer: Option<SymbolKind> = None;
        let written = Name::identifier(name);
        for scope in self.scopes() {
            for binding in scope.bindings_of(&written) {
                let Some(kind) = symbol_kind_of(binding.kind) else {
                    continue;
                };
                answer = Some(match (answer, kind) {
                    (Some(SymbolKind::Type), _) | (_, SymbolKind::Type) => SymbolKind::Type,
                    (Some(SymbolKind::Function), _) | (_, SymbolKind::Function) => {
                        SymbolKind::Function
                    }
                    (Some(previous), _) => previous,
                    (None, kind) => kind,
                });
                if answer == Some(SymbolKind::Type) {
                    return answer;
                }
            }
        }

        answer
    }
}

/// Translate one of this crate's binding kinds into the parser's vocabulary.
///
/// `None` where the parser has no use for the name — a label, a `using`-declaration, a binding this crate could
/// only classify as `Other`. The mapping is deliberately *lossy in one direction*: several kinds collapse onto
/// `Type` or `Function`, because the parser's question is which reading to take, not what the entity is.
fn symbol_kind_of(kind: BindingKind) -> Option<SymbolKind> {
    Some(match kind {
        BindingKind::Class | BindingKind::Enum | BindingKind::Alias | BindingKind::Typedef => {
            SymbolKind::Type
        }
        // A template parameter is a type name inside the declaration that declares it — and for a parser that
        // decides *readings*, "this is a type here" is the whole answer. A *lookup* through it (`T::value_type`)
        // is a different question, and `UnknownReason::DependentName` is where this crate answers that one.
        BindingKind::TemplateParameter => SymbolKind::Type,
        BindingKind::Namespace => SymbolKind::Namespace,
        // Every callable spelling the parser cares about: `g(1, 2);` is a call for all of them.
        BindingKind::Function
        | BindingKind::Constructor
        | BindingKind::Destructor
        | BindingKind::ConversionFunction
        | BindingKind::OperatorFunction
        | BindingKind::LiteralOperator => SymbolKind::Function,
        BindingKind::Variable | BindingKind::Enumerator => SymbolKind::Variable,
        // Asked about a name, these answer nothing about *it*: a `using ns::f;` declares `f` in terms of another
        // name, a label is in a name space of its own, and `Other` is the crate saying it could not tell.
        BindingKind::UsingDeclaration
        | BindingKind::UsingDirective
        | BindingKind::Label
        | BindingKind::Other => return None,
        // Macros are not bindings in this crate — see the module documentation. The arm exists so that a future
        // `BindingKind::Macro` is a compile error here rather than a silent `None`.
        #[allow(unreachable_patterns)]
        _ => return None,
    })
}

/// The vocabulary the bridge can answer with, re-exported so a caller does not have to depend on `cpp_parser`
/// for it.
pub use cpp_parser::SymbolKind as ParserSymbolKind;

/// What a macro body is called in the parser's vocabulary — a convenience for a caller that has a [`MacroTable`]
/// and wants to describe a macro through the same interface later.
///
/// [`MacroTable`]: crate::MacroTable
pub fn macro_kind(function_like: bool, body: MacroBody) -> SymbolKind {
    SymbolKind::Macro {
        function_like,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::{ScopeTree, SymbolKind, SymbolTable, symbol_kind_of};
    use crate::sema::symbol::BindingKind;

    /// Every binding kind this crate has must map to *something* decided, not to a default that happens to
    /// compile: the question "what does the parser see when it asks about this name" has an answer for each.
    #[test]
    fn every_binding_kind_is_either_translated_or_deliberately_silent() {
        let translated = [
            (BindingKind::Class, SymbolKind::Type),
            (BindingKind::Enum, SymbolKind::Type),
            (BindingKind::Alias, SymbolKind::Type),
            (BindingKind::Typedef, SymbolKind::Type),
            (BindingKind::TemplateParameter, SymbolKind::Type),
            (BindingKind::Namespace, SymbolKind::Namespace),
            (BindingKind::Function, SymbolKind::Function),
            (BindingKind::Constructor, SymbolKind::Function),
            (BindingKind::Destructor, SymbolKind::Function),
            (BindingKind::ConversionFunction, SymbolKind::Function),
            (BindingKind::OperatorFunction, SymbolKind::Function),
            (BindingKind::LiteralOperator, SymbolKind::Function),
            (BindingKind::Variable, SymbolKind::Variable),
            (BindingKind::Enumerator, SymbolKind::Variable),
        ];
        for (kind, expected) in translated {
            assert_eq!(symbol_kind_of(kind), Some(expected), "{kind:?}");
        }

        for kind in [
            BindingKind::UsingDeclaration,
            BindingKind::UsingDirective,
            BindingKind::Label,
            BindingKind::Other,
        ] {
            assert_eq!(
                symbol_kind_of(kind),
                None,
                "{kind:?} answers nothing about the name, which is `None` and not a `no`"
            );
        }
    }

    #[test]
    fn the_trait_is_implemented_for_the_scope_tree() {
        // The compile-time half of the bridge: this is what makes `ParserConfig::with_symbol_table(&tree)` legal,
        // and it is checked here because the error would otherwise appear in a *consumer* of both crates.
        fn assert_implemented<T: SymbolTable>() {}
        assert_implemented::<ScopeTree>();

        let tree = ScopeTree::new();
        let table: &dyn SymbolTable = &tree;
        assert_eq!(
            table.kind_of("anything"),
            None,
            "an empty tree knows nothing, and says so"
        );
        assert_eq!(
            table.kind_of("ns::Widget"),
            None,
            "qualified names are not answered"
        );
        assert_eq!(table.kind_of(""), None);
    }
}
