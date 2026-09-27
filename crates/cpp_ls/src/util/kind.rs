//! What kind of thing a declaration is, in the two vocabularies the protocol has for it.
//!
//! `CompletionItemKind` (an icon beside a name in a list) and `SymbolKind` (an icon in an outline or a breadcrumb
//! bar) are different enums with different members, and the analysis has a third: [`DeclKind`], which is what the
//! index stores. Two mappings rather than one, because the target vocabularies really are different — and one
//! module rather than one per handler, because a declaration's kind is one question and two answers to it belong
//! side by side where a reader can see that they agree.
//!
//! **The mapping is deliberately coarse, and in one direction only.** `DeclKind` says what the index
//! distinguishes — six kinds — and the protocol has thirty. `Type` becomes `Class` rather than `Struct`, `Enum` or
//! `Interface` because the fact does not know which of those it is: the declarations layer records `typedef`,
//! `class` and `enum` alike as `Type`. An icon is a hint, and a hint that guessed would be wrong in a way the
//! user cannot check.

use cpp_code_analysis::DeclKind;
use lsp_types::{CompletionItemKind, SymbolKind};

/// The kind of a name offered in a completion list.
pub fn completion_kind(kind: DeclKind) -> CompletionItemKind {
    match kind {
        DeclKind::Type => CompletionItemKind::CLASS,
        DeclKind::Function => CompletionItemKind::FUNCTION,
        DeclKind::Variable => CompletionItemKind::VARIABLE,
        DeclKind::Namespace => CompletionItemKind::MODULE,
        DeclKind::MacroLike => CompletionItemKind::CONSTANT,
        DeclKind::Other => CompletionItemKind::TEXT,
    }
}

/// The kind of a declaration in an outline or a breadcrumb bar.
///
/// The **same** choices as [`completion_kind`], spelled in the other enum — with one difference the vocabularies
/// force: `SymbolKind` has no `TEXT`, so a declaration the index could not classify is `OBJECT`, the least specific
/// member it does have. Naming it something confident (`VARIABLE`, `FUNCTION`) would be a claim about a declaration
/// nothing in the index supports.
pub fn symbol_kind(kind: DeclKind) -> SymbolKind {
    match kind {
        DeclKind::Type => SymbolKind::CLASS,
        DeclKind::Function => SymbolKind::FUNCTION,
        DeclKind::Variable => SymbolKind::VARIABLE,
        DeclKind::Namespace => SymbolKind::MODULE,
        DeclKind::MacroLike => SymbolKind::CONSTANT,
        DeclKind::Other => SymbolKind::OBJECT,
    }
}

#[cfg(test)]
mod tests {
    use super::{completion_kind, symbol_kind};
    use cpp_code_analysis::DeclKind;
    use lsp_types::{CompletionItemKind, SymbolKind};

    /// Every kind the index stores has an icon in both vocabularies, and the match is exhaustive on purpose: a kind
    /// added to `DeclKind` without a row here would not compile, where a `_` arm would quietly hand every client an
    /// icon for the wrong thing.
    #[test]
    fn every_declaration_kind_has_an_icon_in_both_vocabularies() {
        assert_eq!(completion_kind(DeclKind::Type), CompletionItemKind::CLASS);
        assert_eq!(completion_kind(DeclKind::Function), CompletionItemKind::FUNCTION);
        assert_eq!(completion_kind(DeclKind::Variable), CompletionItemKind::VARIABLE);
        assert_eq!(completion_kind(DeclKind::Namespace), CompletionItemKind::MODULE);
        assert_eq!(completion_kind(DeclKind::MacroLike), CompletionItemKind::CONSTANT);
        assert_eq!(completion_kind(DeclKind::Other), CompletionItemKind::TEXT);

        assert_eq!(symbol_kind(DeclKind::Type), SymbolKind::CLASS);
        assert_eq!(symbol_kind(DeclKind::Function), SymbolKind::FUNCTION);
        assert_eq!(symbol_kind(DeclKind::Variable), SymbolKind::VARIABLE);
        assert_eq!(symbol_kind(DeclKind::Namespace), SymbolKind::MODULE);
        assert_eq!(symbol_kind(DeclKind::MacroLike), SymbolKind::CONSTANT);
        assert_eq!(symbol_kind(DeclKind::Other), SymbolKind::OBJECT);
    }
}
