//! C++20 module nodes.
//!
//! `module` and `import` are contextual keywords, so the parser recognises them by spelling at the
//! start of a declaration. The nodes here are what it produces; none of the ordering rules the
//! standard places on module units are represented, because an editor has to parse files that are
//! still being written.

use crate::{
    kind::{CppKind, CppSyntaxKind, CppTokenKind},
    syntax::traits::{CppAstChildren, CppAstNode},
    CppSyntaxNode,
};

use super::{CppDeclaration};

// ============================================================================
// Modules
// ============================================================================

/// A module declaration: `export module my.mod;`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppModuleDecl {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppModuleDecl {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ModuleDecl
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppModuleDecl {
    pub fn get_name(&self) -> Option<CppModuleName> {
        self.child()
    }

    pub fn get_partition(&self) -> Option<CppModulePartition> {
        self.child()
    }

    /// Is this a module *interface* unit (`export module ...`)?
    pub fn is_interface(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::ExportKeyword))
    }
}

/// A dotted module name: `my.mod`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppModuleName {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppModuleName {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ModuleName
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppModuleName {
    /// The name as written, without whitespace.
    pub fn get_name_text(&self) -> String {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .map(|token| token.text().to_string())
            .collect::<Vec<_>>()
            .join("")
    }
}

/// A module partition: `:part`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppModulePartition {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppModulePartition {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ModulePartition
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppModulePartition {
    pub fn get_name(&self) -> Option<CppModuleName> {
        self.child()
    }
}

/// An import declaration: `import std;`, `import :part;`, `import <iostream>;`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppImportDecl {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppImportDecl {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ImportDecl
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppImportDecl {
    pub fn get_name(&self) -> Option<CppModuleName> {
        self.child()
    }

    pub fn get_partition(&self) -> Option<CppModulePartition> {
        self.child()
    }

    /// The header unit name, for `import <iostream>;`.
    pub fn get_header_name(&self) -> Option<CppHeaderName> {
        self.child()
    }

    /// Is this `export import ...;` — a re-export?
    pub fn is_reexport(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::ExportKeyword))
    }

    pub fn is_header_unit(&self) -> bool {
        self.get_header_name().is_some()
    }
}

/// A header unit name: `<iostream>` or `"local.h"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppHeaderName {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppHeaderName {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::HeaderName
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppHeaderName {
    /// The header name without its delimiters.
    pub fn get_name_text(&self) -> String {
        let text = self.syntax().text().to_string();
        text.trim_start_matches(['<', '"'])
            .trim_end_matches(['>', '"'])
            .to_string()
    }

    pub fn is_angle(&self) -> bool {
        self.syntax().text().to_string().starts_with('<')
    }
}

/// An export block: `export { ... }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppExportBlock {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppExportBlock {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ExportBlock
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppExportBlock {
    pub fn get_declarations(&self) -> CppAstChildren<CppDeclaration> {
        self.children()
    }
}

/// A `#` directive, kept as a leaf of the tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppPreprocessorDirective {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppPreprocessorDirective {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::PreprocessorDirective
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppPreprocessorDirective {
    /// The directive name (`include`, `define`, `if`, ...), or `None` for the null directive.
    ///
    /// Read from the token after `#`, which is an identifier — the directive names are not keywords.
    pub fn get_directive_name(&self) -> Option<String> {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .filter(|token| !crate::syntax::node::traits::is_trivia(token.kind().into()))
            .nth(1)
            .map(|token| token.text().to_string())
    }

    /// The header name of an `#include`, when the lexer recognised one.
    ///
    /// This is the header's *text*, not a [`CppHeaderName`] node, because `#include <vector>` is not
    /// a C++ construct: the lexer re-scans `<vector>` into a single `HeaderName` token and the
    /// directive keeps it as a token, so there is no node to hand back. `import <iostream>;` is a
    /// real declaration and does have a node — see [`CppImportDecl::get_header_name`].
    pub fn get_header_name_text(&self) -> Option<String> {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|token| token.kind() == CppKind::Token(CppTokenKind::HeaderName))
            .map(|token| {
                token
                    .text()
                    .trim_start_matches(['<', '"'])
                    .trim_end_matches(['>', '"'])
                    .to_string()
            })
    }

    /// The condition text of a conditional directive, for the preprocessor layer.
    pub fn get_condition_text(&self) -> String {
        let mut seen_hash = false;
        let mut seen_name = false;
        let mut out = Vec::new();

        for token in self
            .syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .filter(|token| !crate::syntax::node::traits::is_trivia(token.kind().into()))
        {
            if !seen_hash {
                seen_hash = true;
                continue;
            }
            if !seen_name {
                seen_name = true;
                continue;
            }
            out.push(token.text().to_string());
        }

        out.join(" ")
    }
}