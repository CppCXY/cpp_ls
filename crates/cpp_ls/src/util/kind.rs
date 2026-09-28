//! What kind of thing a declaration is, in the two vocabularies the protocol has for it.
//!
//! `SymbolKind` (an icon in an outline or a breadcrumb bar) is mapped here, from the analysis's [`DeclKind`].
//! `CompletionItemKind` (an icon beside a name in a list) is **not** here: a completion item's kind comes from the
//! completion layer's own vocabulary (`cpp_code_analysis::ItemKind`), which distinguishes a method from a function
//! and an enumerator from a variable — distinctions [`DeclKind`] does not carry. The mapping lives in
//! `handlers::completion`, beside the only code that knows both vocabularies.
//!
//! **The mapping here is deliberately coarse, and in one direction only.** `DeclKind` says what the index
//! distinguishes — six kinds — and the protocol has thirty. `Type` becomes `Class` rather than `Struct`, `Enum` or
//! `Interface` because the fact does not know which of those it is: the declarations layer records `typedef`,
//! `class` and `enum` alike as `Type`. An icon is a hint, and a hint that guessed would be wrong in a way the
//! user cannot check.

use cpp_code_analysis::DeclKind;
use lsp_types::SymbolKind;

/// The kind of a declaration in an outline or a breadcrumb bar.
///
/// `SymbolKind` has no `TEXT`, so a declaration the index could not classify is `OBJECT`, the least specific member
/// it does have. Naming it something confident (`VARIABLE`, `FUNCTION`) would be a claim about a declaration
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
    use super::symbol_kind;
    use cpp_code_analysis::DeclKind;
    use lsp_types::SymbolKind;

    /// Every kind the index stores has an icon, and the match is exhaustive on purpose: a kind added to `DeclKind`
    /// without a row here would not compile, where a `_` arm would quietly hand every client an icon for the wrong
    /// thing.
    #[test]
    fn every_declaration_kind_has_an_icon() {
        assert_eq!(symbol_kind(DeclKind::Type), SymbolKind::CLASS);
        assert_eq!(symbol_kind(DeclKind::Function), SymbolKind::FUNCTION);
        assert_eq!(symbol_kind(DeclKind::Variable), SymbolKind::VARIABLE);
        assert_eq!(symbol_kind(DeclKind::Namespace), SymbolKind::MODULE);
        assert_eq!(symbol_kind(DeclKind::MacroLike), SymbolKind::CONSTANT);
        assert_eq!(symbol_kind(DeclKind::Other), SymbolKind::OBJECT);
    }
}
