//! Declaration, type and module nodes.

use crate::{
    CppSyntaxNode,
    kind::{CppKind, CppSyntaxKind, CppTokenKind},
    syntax::traits::{CppAstChildren, CppAstNode, CppAstToken},
};

use super::{CppKeywordToken, CppNameToken, CppPunctuationToken, CppStat};

mod expr;
mod modules;
mod stat;

pub use expr::*;
pub use modules::*;
pub use stat::*;

// ============================================================================
// Root
// ============================================================================

/// The root of a parsed file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTranslationUnit {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTranslationUnit {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::TranslationUnit
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTranslationUnit {
    /// Every top-level declaration and directive, in order.
    ///
    /// Namespaces are *not* included: `namespace ns { ... }` is a [`CppNamespaceDecl`], a different
    /// node kind, so iterate [`CppTranslationUnit::get_namespaces`] for those.
    pub fn get_declarations(&self) -> CppAstChildren<CppDeclaration> {
        self.children()
    }

    /// Every top-level namespace definition, in order.
    pub fn get_namespaces(&self) -> CppAstChildren<CppNamespaceDecl> {
        self.children()
    }

    pub fn get_module_decl(&self) -> Option<CppModuleDecl> {
        self.child()
    }
}

// ============================================================================
// Declarations
// ============================================================================

/// One declaration: `specifiers declarators ;`, a function definition, a class, a namespace, ...
///
/// A single type rather than one per declaration form, because the forms differ only in which child
/// nodes are present. Use the `get_*` accessors — or [`CppDeclaration::kind_name`] — to see which.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDeclaration {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDeclaration {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    /// Narrow on purpose, unlike [`CppDeclaration::cast`].
    ///
    /// `can_cast` is what [`CppAstChildren`](crate::syntax::traits::CppAstChildren) uses on every step
    /// of an iteration, so widening it would put every `using` directive — including
    /// `using namespace std;`, which declares nothing — into
    /// [`CppTranslationUnit::get_declarations`].
    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::Declaration
    }

    /// Wider than [`CppDeclaration::can_cast`]: it also accepts a `using` alias, which declares a name
    /// but has a shape that is not `specifiers declarators` and therefore its own node kind.
    ///
    /// This is the same asymmetry [`CppEnumDef`] has — a deliberate cast target that is not reachable
    /// by iterating children — and it exists because the two questions differ. "List this file's
    /// declarations" should not be polluted by directives; "what declaration does this comment
    /// document?" must not miss `/// Doc.` in front of `using Point = shapes::Point;`.
    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        matches!(
            CppSyntaxKind::from(syntax.kind()),
            CppSyntaxKind::Declaration | CppSyntaxKind::UsingDecl
        )
        .then_some(Self { syntax })
    }
}

impl CppDeclaration {
    /// The `decl-specifier-seq`: the type and the specifiers.
    pub fn get_decl_specifiers(&self) -> Option<CppDeclSpecifierSeq> {
        self.child()
    }

    /// Every `init-declarator`. A declaration of several entities has several of these.
    pub fn get_init_declarators(&self) -> CppAstChildren<CppInitDeclarator> {
        self.children()
    }

    /// The first `init-declarator`, which is the one every declaration has.
    pub fn get_init_declarator(&self) -> Option<CppInitDeclarator> {
        self.child()
    }

    /// The name this declaration introduces, if it has one.
    ///
    /// Four places a name can live, and they are tried in order:
    ///
    /// * a class, struct, union or enum definition — the name is on the definition node;
    /// * a namespace — same;
    /// * everything else — the name is the first declarator's;
    /// * a **forward declaration** — `class Widget;` — has no declarator at all, and the name is the type
    ///   written beside the class-like keyword. See [`CppDeclaration::elaborated_type_name`].
    ///
    /// Taken from the first declarator, which is what almost every caller wants. A declaration of
    /// several entities (`int a, b;`) has more — use `get_init_declarators` for those.
    pub fn get_name(&self) -> Option<CppNameToken> {
        // A `using` alias: the name comes before the `=`, so it is the first `NameExpr` child. The
        // target is a second one, nested inside a `TypeId`, which is why this is the *child* search
        // rather than a descendant one.
        if CppSyntaxKind::from(self.syntax.kind()) == CppSyntaxKind::UsingDecl {
            return crate::syntax::node::traits::first_child_of_kind(
                self.syntax(),
                &[CppSyntaxKind::NameExpr],
            )
            .and_then(CppNameExpr::cast)
            .and_then(|name| name.get_name_token());
        }

        if let Some(class) = self.get_class_def()
            && let Some(name) = class.get_name()
        {
            return Some(name);
        }

        if let Some(enum_def) = self.get_enum_def()
            && let Some(name) = enum_def.get_name()
        {
            return Some(name);
        }

        // A forward declaration — `class Widget;` — names the class through its *specifier*: there is no
        // declarator, and the keyword and the name are all the declaration has. It is the elaborated-type-
        // specifier spelling, so the name to report is the type written after the keyword.
        //
        // Checked after the declarator, not before: `class Widget x;` declares the *variable* `x` and mentions
        // `Widget` as its type, and the declarator is the answer there.
        if let Some(declarator) = self.get_init_declarator()
            && let Some(name) = declarator.get_name()
        {
            return Some(name);
        }

        self.elaborated_type_name()
    }

    /// The class name of an elaborated type specifier: the `Widget` of `class Widget;`.
    ///
    /// `None` for every declaration whose specifiers are not a class-like keyword followed by a name, which is
    /// what keeps this from answering for `class { ... } x;` — an unnamed class with a declarator — or for
    /// `int x;`.
    fn elaborated_type_name(&self) -> Option<CppNameToken> {
        let specifiers = self.get_decl_specifiers()?;

        let builtin = specifiers.get_builtin_type();
        let has_class_keyword = builtin.as_ref().is_some_and(|builtin| builtin.is_class_like());
        if !has_class_keyword {
            return None;
        }

        specifiers
            .get_type_name()
            .and_then(|name| name.get_name_token())
    }

    /// The text of the declared name.
    pub fn get_name_text(&self) -> Option<String> {
        self.get_name().map(|it| it.get_name_text().to_string())
    }

    /// The declarator of the first `init-declarator`.
    pub fn get_declarator(&self) -> Option<CppDeclarator> {
        self.get_init_declarator()?.get_declarator()
    }

    /// The initializer of the first `init-declarator`, if it has one.
    pub fn get_initializer(&self) -> Option<CppInitializer> {
        self.get_init_declarator()?.get_initializer()
    }

    /// The function body, if this declaration is a function definition.
    ///
    /// The body is a direct child of the declaration, not of the declarator: `int f() { ... }` is one
    /// declaration whose last init-declarator is followed by a block.
    pub fn get_body(&self) -> Option<CppCompoundStat> {
        self.child()
    }

    /// The class, struct, union or enum this declaration defines, if any.
    pub fn get_class_def(&self) -> Option<CppClassDef> {
        self.get_decl_specifiers()?.get_class_def()
    }

    /// The enum this declaration defines, if any.
    ///
    /// A scoped enum (`enum class E`) is parsed as a single `BuiltinType` specifier rather than as an
    /// `EnumDef`, because `class` belongs to the same specifier as `enum` — so the search has to be
    /// by *shape* rather than by direct child. [`CppEnumDef::can_cast`] accepts both node kinds, and
    /// only a node that actually has a body counts as a definition; an elaborated type specifier
    /// (`enum E x;`) has no body and yields `None`.
    pub fn get_enum_def(&self) -> Option<CppEnumDef> {
        self.get_decl_specifiers()?
            .syntax()
            .descendants()
            .filter_map(CppEnumDef::cast)
            .find(|it| it.get_body().is_some())
    }

    pub fn get_namespace_decl(&self) -> Option<CppNamespaceDecl> {
        self.child()
    }

    /// The template head, if this declaration is templated.
    pub fn get_template_decl(&self) -> Option<CppTemplateDecl> {
        self.child()
    }

    /// Is this a function definition (a declarator with a parameter list plus a body)?
    pub fn is_function_def(&self) -> bool {
        self.get_body().is_some() && self.get_declarator().is_some_and(|it| it.is_function())
    }

    /// Is this declaration exported? (`export` is a token, so this is a token test, not a node test.)
    pub fn is_exported(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::ExportKeyword))
    }

    /// Which of the declaration forms this is, for diagnostics and tests.
    ///
    /// A namespace is not one of the answers: `namespace ns { ... }` is a `NamespaceDecl` node, not
    /// a `Declaration`, so it never reaches here — ask the translation unit for namespaces.
    pub fn kind_name(&self) -> &'static str {
        // A `using` alias is its own node kind, reached through `CppDeclaration::cast` for consumers
        // that ask "what does this document?" rather than by iterating declarations. It is an alias
        // for the same reason `using A = B;` is, so it reports the same word.
        if CppSyntaxKind::from(self.syntax.kind()) == CppSyntaxKind::UsingDecl {
            "alias"
        } else if self.get_enum_def().is_some() {
            "enum"
        } else if self.get_class_def().is_some() {
            "class"
        } else if self.get_declarator().is_some_and(|it| it.is_function()) {
            // A parameter list is what makes a declaration a function — a body is only what makes it
            // a *definition*, and `void reset();` is a function declaration.
            "function"
        } else if self.get_decl_specifiers().is_some_and(|it| it.is_alias()) {
            "alias"
        } else {
            "variable"
        }
    }
}

/// The `decl-specifier-seq` of a declaration.
///
/// Deliberately flat: C++ allows the specifiers in almost any order and repeats them freely
/// (`long long unsigned int`), so anything more structured would invent an order the language does
/// not have. Consumers ask for what they want — the base type, the qualifiers, whether it is
/// `static` — through the accessors here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDeclSpecifierSeq {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDeclSpecifierSeq {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DeclSpecifierSeq
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDeclSpecifierSeq {
    /// The built-in type specifier, if the type is a fundamental one (`int`, `const char`, ...).
    pub fn get_builtin_type(&self) -> Option<CppBuiltinType> {
        self.child()
    }

    /// The user-written type name, if the type is a name (`Foo`, `std::vector<int>`).
    ///
    /// Two spellings reach here: a plain name is a `NameExpr` child, while a template-id
    /// (`std::vector<int>`) is wrapped in a `TemplateType` node with the `NameExpr` inside it. Both
    /// are "the name the user wrote", so both are accepted.
    pub fn get_type_name(&self) -> Option<CppNameExpr> {
        if let Some(name) = self.child::<CppNameExpr>() {
            return Some(name);
        }

        let template = crate::syntax::node::traits::first_child_of_kind(
            self.syntax(),
            &[CppSyntaxKind::TemplateType],
        )?;
        template.children().find_map(CppNameExpr::cast)
    }

    /// The class-like definition this specifier sequence defines, if any.
    pub fn get_class_def(&self) -> Option<CppClassDef> {
        self.child()
    }

    pub fn get_enum_def(&self) -> Option<CppEnumDef> {
        self.child()
    }

    /// The `=` of a `using Alias = type;`.
    pub fn is_alias(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::Assign))
    }

    /// Is one of these keywords present in the specifier sequence?
    pub fn has_keyword(&self, kind: CppTokenKind) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == kind.into())
    }

    pub fn is_const(&self) -> bool {
        self.has_keyword(CppTokenKind::ConstKeyword)
    }

    pub fn is_static(&self) -> bool {
        self.has_keyword(CppTokenKind::StaticKeyword)
    }

    pub fn is_virtual(&self) -> bool {
        self.has_keyword(CppTokenKind::VirtualKeyword)
    }

    /// The text of the whole specifier sequence, trivia removed.
    ///
    /// Useful for presenting a type to a user without building a type model first. Walks the whole
    /// subtree, because the type itself is a child node (`BuiltinType`, `TemplateType`), not a token
    /// of this node.
    pub fn get_type_text(&self) -> String {
        crate::syntax::node::traits::subtree_text_spaced(self.syntax())
    }
}

/// A built-in type specifier: `int`, `const char`, `decltype(x)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppBuiltinType {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppBuiltinType {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::BuiltinType
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppBuiltinType {
    /// The keyword, if the specifier is a single keyword.
    pub fn get_keyword(&self) -> Option<CppKeywordToken> {
        self.token()
    }

    pub fn get_type_text(&self) -> String {
        crate::syntax::node::traits::subtree_text_spaced(self.syntax())
    }

    /// Is this specifier a class-like keyword — `class`, `struct`, `union` or `enum`?
    ///
    /// Asked by callers that need to tell an **elaborated type specifier** — `class Widget;`, where the keyword
    /// names a kind of type and the name after it is the type — from every other use of those keywords, where a
    /// body or a base clause follows and the keyword is the head of a definition.
    pub fn is_class_like(&self) -> bool {
        matches!(
            self.syntax().children_with_tokens().next().and_then(|element| element.into_token()).map(|token| token.kind()),
            Some(CppKind::Token(CppTokenKind::ClassKeyword))
                | Some(CppKind::Token(CppTokenKind::StructKeyword))
                | Some(CppKind::Token(CppTokenKind::UnionKeyword))
                | Some(CppKind::Token(CppTokenKind::EnumKeyword))
        )
    }
}

/// The `type-id` of a cast, `sizeof`, parameter or template argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTypeId {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTypeId {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::TypeId | CppSyntaxKind::TrailingReturnType
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTypeId {
    pub fn get_decl_specifiers(&self) -> Option<CppDeclSpecifierSeq> {
        self.child()
    }

    pub fn get_type_text(&self) -> String {
        let text = self.syntax().text().to_string();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// A `declarator`: the part that names the entity, possibly wrapped in pointers, references, arrays,
/// function parameter lists and template argument lists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDeclarator {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDeclarator {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::Declarator
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDeclarator {
    /// The declared name.
    ///
    /// Found among the child nodes rather than the direct tokens: a qualified or templated name
    /// (`ns::Foo<int>`) is itself a node, so the identifier is one level down.
    pub fn get_name(&self) -> Option<CppNameToken> {
        self.token().or_else(|| {
            self.child::<CppNameExpr>()
                .and_then(|it| it.get_name_token())
        })
    }

    pub fn get_name_text(&self) -> Option<String> {
        self.get_name().map(|it| it.get_name_text().to_string())
    }

    /// The full spelling of this declarator including the name, trivia removed.
    pub fn get_declarator_text(&self) -> String {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .filter(|token| !crate::syntax::node::traits::is_trivia(token.kind().into()))
            .map(|token| token.text().to_string())
            .collect::<Vec<_>>()
            .join("")
            .chars()
            .filter(|it| !it.is_whitespace())
            .collect()
    }

    /// The parameter list, if this declarator declares a function.
    pub fn get_param_list(&self) -> Option<CppParamList> {
        self.child()
    }

    /// The trailing return type after `->`, if this declarator has one.
    ///
    /// The returned node is the `TypeId` inside the `TrailingReturnType` wrapper, so its text is the
    /// type as written (`int*`) and does not include the `->`.
    pub fn get_trailing_return_type(&self) -> Option<CppTypeId> {
        let wrapper = crate::syntax::node::traits::first_child_of_kind(
            self.syntax(),
            &[CppSyntaxKind::TrailingReturnType],
        );

        match wrapper {
            Some(wrapper) => wrapper.children().find_map(CppTypeId::cast),
            // A grammar that puts the `TypeId` directly under the declarator is still understood.
            None => crate::syntax::node::traits::first_child_of_kind(
                self.syntax(),
                &[CppSyntaxKind::TypeId],
            )
            .and_then(CppTypeId::cast),
        }
    }

    /// The first pointer/reference operator, if any.
    pub fn get_pointer_type(&self) -> Option<CppPointerType> {
        self.child()
    }

    /// Is this a function declarator?
    ///
    /// True when there is a parameter list or a trailing return type. This is the test that decides
    /// whether a following `{` is a function body or a brace initializer.
    pub fn is_function(&self) -> bool {
        self.get_param_list().is_some() || self.get_trailing_return_type().is_some()
    }

    /// Is this an array declarator?
    pub fn is_array(&self) -> bool {
        self.get_array_type().is_some()
    }

    pub fn get_array_type(&self) -> Option<CppArrayType> {
        self.child()
    }

    /// The structured binding this declarator introduces, if it is one: the `[a, b]` of
    /// `auto [a, b] = pair;`.
    ///
    /// Two shapes reach here, because the `&` of `auto& [k, v]` is an abstract declarator and the
    /// pattern comes after it: the `StructuredBinding` is either a child of this declarator or a
    /// child of the `InitDeclarator` beside it. Both are searched, so a caller asking "is this a
    /// binding?" gets one answer rather than having to know which spelling produced the node.
    pub fn get_structured_binding(&self) -> Option<CppStructuredBinding> {
        if let Some(binding) = self.child::<CppStructuredBinding>() {
            return Some(binding);
        }

        self.syntax()
            .parent()
            .and_then(|parent| {
                crate::syntax::node::traits::first_child_of_kind(
                    &parent,
                    &[CppSyntaxKind::StructuredBinding],
                )
            })
            .and_then(CppStructuredBinding::cast)
    }

    /// Is this a structured binding declarator?
    pub fn is_structured_binding(&self) -> bool {
        self.get_structured_binding().is_some()
    }

    /// The names a structured binding introduces. Empty for an ordinary declarator.
    pub fn get_binding_names(&self) -> Vec<CppNameToken> {
        self.get_structured_binding()
            .map(|binding| binding.get_names().collect())
            .unwrap_or_default()
    }
}

/// A structured binding's name list: the `[a, b]` of `auto [a, b] = pair;`.
///
/// The names are `NameExpr` children rather than declarators, because that is the node a name
/// reference gets — so "which names does this declaration introduce?" is one query over one kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppStructuredBinding {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppStructuredBinding {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::StructuredBinding
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppStructuredBinding {
    /// The identifiers bound, in order.
    ///
    /// A pack expansion (`[... xs]`) contributes the names it names and nothing for the `...`: what it
    /// expands to is a semantic question, and a syntactic layer that guessed would be wrong for every
    /// pack whose size is not yet known.
    pub fn get_names(&self) -> impl Iterator<Item = CppNameToken> {
        self.syntax()
            .descendants()
            .filter_map(CppNameExpr::cast)
            .filter_map(|name| name.get_name_token())
    }

    /// The names as text.
    pub fn get_name_texts(&self) -> Vec<String> {
        self.get_names()
            .map(|name| name.get_name_text().to_string())
            .collect()
    }

    /// How many names the pattern binds.
    pub fn len(&self) -> usize {
        self.get_names().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Is any of the names a pack expansion (`...`)?
    pub fn is_pack_expansion(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::Ellipsis))
    }
}

/// An `init-declarator`: a declarator plus an optional initializer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppInitDeclarator {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppInitDeclarator {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::InitDeclarator
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppInitDeclarator {
    pub fn get_declarator(&self) -> Option<CppDeclarator> {
        self.child()
    }

    pub fn get_name(&self) -> Option<CppNameToken> {
        // A structured binding has no single name: `auto [a, b] = pair;` declares two. The declarator
        // is where the pattern lives, so this answers with the first of them rather than with the
        // rendered pattern, and [`CppDeclarator::get_binding_names`] is the accessor for all of them.
        if let Some(binding) = self.get_structured_binding() {
            return binding.get_names().next();
        }

        self.get_declarator()?.get_name()
    }

    pub fn get_initializer(&self) -> Option<CppInitializer> {
        self.child()
    }

    /// The structured binding this init-declarator introduces, in either spelling.
    ///
    /// `auto [a, b] = pair;` puts the pattern directly under the init-declarator, while
    /// `auto& [k, v] = map;` puts it inside the declarator, after the `&`. This is the one place that
    /// knows both, so a caller asking "is this a binding?" gets one answer.
    pub fn get_structured_binding(&self) -> Option<CppStructuredBinding> {
        if let Some(binding) = self.child::<CppStructuredBinding>() {
            return Some(binding);
        }

        self.get_declarator()?.get_structured_binding()
    }

    pub fn is_structured_binding(&self) -> bool {
        self.get_structured_binding().is_some()
    }
}

/// What follows `=`, or a brace-or-equal initializer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppInitializer {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppInitializer {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::Initializer
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppInitializer {
    /// The initializing expression, if it is an expression rather than a braced list.
    pub fn get_expr(&self) -> Option<CppExpr> {
        self.child()
    }

    /// The braced initializer list, if this is `{...}`.
    pub fn get_init_list(&self) -> Option<CppInitListExpr> {
        self.child()
    }
}

/// A pointer or reference operator in a declarator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppPointerType {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppPointerType {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::PointerType
                | CppSyntaxKind::ReferenceType
                | CppSyntaxKind::RValueReferenceType
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppPointerType {
    pub fn is_rvalue_reference(&self) -> bool {
        CppSyntaxKind::from(self.syntax().kind()) == CppSyntaxKind::RValueReferenceType
    }

    pub fn is_reference(&self) -> bool {
        matches!(
            CppSyntaxKind::from(self.syntax().kind()),
            CppSyntaxKind::ReferenceType | CppSyntaxKind::RValueReferenceType
        )
    }

    pub fn get_operator_token(&self) -> Option<CppPunctuationToken> {
        self.token()
    }
}

/// An array declarator: `arr[10]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppArrayType {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppArrayType {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ArrayType
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppArrayType {
    /// The array bound, if it is written. `int a[]` has none.
    pub fn get_size_expr(&self) -> Option<CppExpr> {
        self.child()
    }
}

/// A function parameter list, including its parentheses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppParamList {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppParamList {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ParameterList
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppParamList {
    pub fn get_params(&self) -> CppAstChildren<CppParam> {
        self.children()
    }

    /// Is the list `(...)` — an old-style variadic function?
    pub fn is_variadic(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::Ellipsis))
    }
}

/// One function parameter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppParam {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppParam {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::Parameter
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppParam {
    pub fn get_decl_specifiers(&self) -> Option<CppDeclSpecifierSeq> {
        self.child()
    }

    pub fn get_declarator(&self) -> Option<CppDeclarator> {
        self.child()
    }

    /// The parameter name. Unnamed parameters (`void f(int)`) have none.
    pub fn get_name(&self) -> Option<CppNameToken> {
        self.get_declarator()?.get_name()
    }

    pub fn get_default_value(&self) -> Option<CppInitializer> {
        self.child()
    }
}

// ============================================================================
// Class-like definitions
// ============================================================================

/// A class, struct or union definition: `class Foo : public Bar { ... };`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppClassDef {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppClassDef {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::ClassDef | CppSyntaxKind::StructDef | CppSyntaxKind::UnionDef
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppClassDef {
    /// The class name. Anonymous classes have none.
    pub fn get_name(&self) -> Option<CppNameToken> {
        self.child::<CppNameExpr>()
            .and_then(|it| it.get_name_token())
    }

    pub fn get_name_text(&self) -> Option<String> {
        self.get_name().map(|it| it.get_name_text().to_string())
    }

    pub fn is_struct(&self) -> bool {
        CppSyntaxKind::from(self.syntax().kind()) == CppSyntaxKind::StructDef
    }

    pub fn is_union(&self) -> bool {
        CppSyntaxKind::from(self.syntax().kind()) == CppSyntaxKind::UnionDef
    }

    /// The body, including its access specifier sections.
    pub fn get_body(&self) -> Option<CppClassBody> {
        self.child()
    }

    /// The base classes.
    pub fn get_base_specifiers(&self) -> CppAstChildren<CppBaseSpecifier> {
        self.children()
    }
}

/// The `{ ... }` of a class definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppClassBody {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppClassBody {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::ClassBody
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppClassBody {
    /// The member declarations of the class.
    pub fn get_members(&self) -> CppAstChildren<CppDeclaration> {
        self.children()
    }

    /// The access specifier sections: `public:`, `private:`, `protected:`.
    pub fn get_access_specifiers(&self) -> Vec<CppAccessSpecifier> {
        self.syntax()
            .children()
            .filter_map(CppAccessSpecifier::cast)
            .collect()
    }
}

/// An access specifier section header: `public:`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppAccessSpecifier {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppAccessSpecifier {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::PublicAccess
                | CppSyntaxKind::PrivateAccess
                | CppSyntaxKind::ProtectedAccess
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppAccessSpecifier {
    pub fn get_keyword(&self) -> Option<CppKeywordToken> {
        self.token()
    }
}

/// A base class in a class definition's base clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppBaseSpecifier {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppBaseSpecifier {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::BaseSpecifier
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppBaseSpecifier {
    pub fn get_name(&self) -> Option<CppNameExpr> {
        self.child()
    }

    pub fn is_virtual(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::VirtualKeyword))
    }

    pub fn get_access(&self) -> Option<CppAccessSpecifier> {
        self.child()
    }
}

/// An enum definition: `enum class Color : int { Red, Green };`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppEnumDef {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppEnumDef {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::EnumDef | CppSyntaxKind::EnumClassDef | CppSyntaxKind::BuiltinType
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppEnumDef {
    /// The `{ ... }` body holding the enumerators, if this really is an enum definition.
    ///
    /// This is what separates `enum class E { ... }` from an elaborated type specifier such as
    /// `enum E x;`: both are `BuiltinType` nodes, and only one of them defines anything.
    pub fn get_body(&self) -> Option<CppSyntaxNode> {
        self.syntax()
            .children()
            .find(|it| CppSyntaxKind::from(it.kind()) == CppSyntaxKind::CompoundStat)
    }

    /// The `enum` keyword, if this node really is an enum.
    pub fn get_enum_keyword(&self) -> Option<CppKeywordToken> {
        self.syntax()
            .descendants_with_tokens()
            .filter_map(|it| it.into_token())
            .filter_map(CppKeywordToken::cast)
            .find(|it| it.get_keyword_kind() == CppKind::Token(CppTokenKind::EnumKeyword))
    }

    /// The enum's name.
    ///
    /// Searched as a subtree: `enum class E` is parsed as one `BuiltinType` specifier (the `class`
    /// belongs to the same specifier as `enum`), so the name sits one level down.
    pub fn get_name(&self) -> Option<CppNameToken> {
        self.get_enum_keyword()?;
        self.syntax()
            .descendants()
            .find_map(CppNameExpr::cast)
            .and_then(|it| it.get_name_token())
    }

    /// Is this a scoped enum (`enum class` / `enum struct`)?
    pub fn is_scoped(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| {
                matches!(
                    token.kind(),
                    CppKind::Token(CppTokenKind::ClassKeyword)
                        | CppKind::Token(CppTokenKind::StructKeyword)
                )
            })
    }

    /// Every enumerator, in order.
    ///
    /// The enumerators live inside the body node rather than directly under the enum, so this walks
    /// one level down instead of using `children()` — which is why it needs `first_child_of_kind`
    /// rather than `child::<N>()`: the body is a `CompoundStat`, the same kind a block statement
    /// uses, so there is no distinct type to ask for.
    pub fn get_enumerators(&self) -> impl Iterator<Item = CppEnumerator> {
        crate::syntax::node::traits::first_child_of_kind(
            self.syntax(),
            &[CppSyntaxKind::CompoundStat],
        )
        .into_iter()
        .flat_map(|body| CppAstChildren::<CppEnumerator>::new(&body))
    }
}

/// One enumerator: `Red` or `Green = 2`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppEnumerator {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppEnumerator {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::EnumeratorDecl
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppEnumerator {
    pub fn get_name(&self) -> Option<CppNameToken> {
        self.token()
    }

    pub fn get_value_expr(&self) -> Option<CppExpr> {
        self.child()
    }
}

// ============================================================================
// Namespaces and templates
// ============================================================================

/// A namespace definition or an alias: `namespace a { ... }`, `namespace fs = std::filesystem;`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppNamespaceDecl {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppNamespaceDecl {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::NamespaceDecl
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppNamespaceDecl {
    pub fn get_name(&self) -> Option<CppNameExpr> {
        self.child()
    }

    /// Everything declared inside the namespace, in order.
    ///
    /// The sum type rather than [`CppDeclaration`], because a namespace body holds more than plain
    /// declarations: `template <...> class Grid { ... };` is a `TemplateDecl` at this level, and
    /// asking for `CppDeclaration` alone silently drops every template in the namespace.
    ///
    /// `namespace ns { ... }` holds a `CompoundStat` body, because a namespace body is
    /// grammatically a block and C++ makes declarations a kind of statement. This walks one level
    /// down so callers do not have to.
    pub fn get_declarations(&self) -> impl Iterator<Item = CppStat> {
        self.get_body()
            .into_iter()
            .flat_map(|body| body.get_stats())
    }

    /// The body block, if this is a definition rather than an alias.
    pub fn get_body(&self) -> Option<CppCompoundStat> {
        self.child()
    }

    /// Is this `namespace name = target;`?
    pub fn is_alias(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == CppKind::Token(CppTokenKind::Assign))
    }
}

/// A template head: `template <typename T, int N>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTemplateDecl {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTemplateDecl {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::TemplateDecl
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTemplateDecl {
    pub fn get_param_list(&self) -> Option<CppTemplateParamList> {
        self.child()
    }

    /// Is this `template <>` — an explicit specialization?
    pub fn is_explicit_specialization(&self) -> bool {
        self.get_param_list().is_none()
    }
}

/// A template parameter list, including its angle brackets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTemplateParamList {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTemplateParamList {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::TemplateParameterList
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTemplateParamList {
    pub fn get_params(&self) -> CppAstChildren<CppTemplateParam> {
        self.children()
    }
}

/// One template parameter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTemplateParam {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTemplateParam {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::TemplateParameter
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTemplateParam {
    pub fn get_decl_specifiers(&self) -> Option<CppDeclSpecifierSeq> {
        self.child()
    }

    pub fn get_name(&self) -> Option<CppNameToken> {
        self.get_declarator()?.get_name()
    }

    pub fn get_declarator(&self) -> Option<CppDeclarator> {
        self.child()
    }

    /// Is this `typename T` / `class T`?
    pub fn is_type_param(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| {
                matches!(
                    token.kind(),
                    CppKind::Token(CppTokenKind::TypenameKeyword)
                        | CppKind::Token(CppTokenKind::ClassKeyword)
                )
            })
    }
}

/// A template argument list, including its angle brackets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTemplateArgList {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTemplateArgList {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::TemplateArgumentList
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTemplateArgList {
    pub fn get_args(&self) -> CppAstChildren<CppTemplateArg> {
        self.children()
    }
}

/// One template argument: a type or a constant expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTemplateArg {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppTemplateArg {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::TemplateArgument
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppTemplateArg {
    pub fn get_type_id(&self) -> Option<CppTypeId> {
        self.child()
    }

    pub fn get_expr(&self) -> Option<CppExpr> {
        self.child()
    }
}
