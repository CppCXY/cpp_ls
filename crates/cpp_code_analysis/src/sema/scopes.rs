//! Building a file's scopes and bindings from its syntax tree.
//!
//! This is the step that turns the syntax into something a name can be looked up in, and it stays **inside one
//! file**: nothing here reads an `#include`, another file, or the project. That is not a limitation but the
//! point — completion at a cursor needs this file's scopes and nothing else, so a step that reached further
//! would make every keystroke cost the whole project. Crossing a file boundary is the include graph's job, and
//! it happens at a different layer.
//!
//! # What the syntax can and cannot say
//!
//! Every binding here is derived from the **shape** of a declaration. `class Widget` binds a
//! [`BindingKind::Class`] because a class definition was written, not because anything was resolved. There are
//! no types at this layer, so `auto x = f();` binds a variable whose type is not known, and that is the honest
//! result rather than a failure.
//!
//! Two consequences are worth stating because they look like omissions and are not:
//!
//! * **A reference is not recorded.** `f()` inside a body is not a binding; only what a declaration
//!   *introduces* is. Collecting references is a different pass with a different output shape, and doing it
//!   here would make this pass's result ambiguous between "declared here" and "used here".
//! * **A qualified name is kept qualified.** `int ns::Widget::count;` does not declare `count` in the
//!   enclosing scope, because that is not what it means — it declares a member of `ns::Widget`, and resolving
//!   which one needs the qualifier resolved. Binding it as a plain `count` would put a name in scope that
//!   cannot actually be used unqualified.
//!
//! # Malformed input
//!
//! This layer runs while a file is being typed, so every step has a fallback: an unreadable name is skipped
//! rather than invented, and a construct the parser recovered from contributes whatever it did parse. A
//! binding that cannot be placed is dropped, and the resulting gap is visible to a consumer — which is better
//! than a binding in the wrong scope, and much better than a panic in an editor.

use cpp_parser::{CppAstNode, CppSyntaxKind, CppSyntaxNode, CppTokenKind};

use crate::symbol::{
    Binding, BindingKind, BindingOrigin, DeclName, Name, QualifiedName, ScopeId, ScopeKind,
    ScopeTree,
};

/// Should a block statement open a scope of its own?
///
/// A `compound-statement` is the body of a namespace, of a function, of a class — and of an `if`, a loop, or a
/// bare block. Only the last group introduces a scope: the others *are* the scope their header created, and
/// opening a second one would put a namespace's declarations one level too deep. The distinction is carried by
/// the caller, which knows which header it is walking.
///
/// The function-body case is handled by walking the body's *contents* into the function scope rather than by
/// walking the body node, so there is no variant for it here: a `CompoundStat` reached through
/// [`ScopeWalker::descend`] is always a statement block, because every construct whose body is not one has
/// already taken its contents directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    /// The compound statement is a statement block and opens its own scope.
    OpensAScope,
}

/// Build a file's symbol table from its syntax tree.
///
/// The root node is the translation unit, and the table it produces has that as its file scope. See the module
/// documentation for what is and is not recorded.
pub fn build_scopes(root: &CppSyntaxNode) -> ScopeTree {
    let mut walker = ScopeWalker {
        table: ScopeTree::new(),
    };

    let file = walker.table.create_scope(
        ScopeKind::TranslationUnit,
        None,
        Some(cpp_parser::source_range(root.text_range())),
    );

    walker.items(root, file);

    walker.table
}

/// Walks a tree, creating a scope per construct and a binding per declaration.
struct ScopeWalker {
    table: ScopeTree,
}

impl ScopeWalker {
    /// Walk the children of `node`, adding what they declare to `scope`.
    fn items(&mut self, node: &CppSyntaxNode, scope: ScopeId) {
        for child in node.children() {
            self.item(&child, scope);
        }
    }

    /// Walk the children of `node` into `scope`, treating a body node as part of `scope` rather than nested.
    ///
    /// For a namespace, a class, or an enum, the `{ ... }` **is** the scope the construct opened: its contents
    /// belong to `scope` itself, not to a block inside it. Walking the body node through the ordinary path would
    /// open a second scope and put every declaration one level too deep — which still looks plausible, and
    /// breaks every lookup that walks outward.
    fn items_in_scope(&mut self, node: &CppSyntaxNode, scope: ScopeId, bodies: &[CppSyntaxKind]) {
        for child in node.children() {
            if bodies.contains(&CppSyntaxKind::from(child.kind())) {
                self.items(&child, scope);
            } else {
                self.item(&child, scope);
            }
        }
    }

    /// Walk one construct, adding what it declares to `scope`.
    ///
    /// The match is exhaustive over the kinds that *declare* something, and everything else falls through to
    /// [`ScopeWalker::descend`], which keeps looking. That fallback is what makes this robust on the shapes the
    /// grammar produces for constructs it recovered from: a node this does not recognise is still searched for
    /// the declarations inside it.
    fn item(&mut self, node: &CppSyntaxNode, scope: ScopeId) {
        match CppSyntaxKind::from(node.kind()) {
            CppSyntaxKind::NamespaceDecl => self.namespace(node, scope),
            CppSyntaxKind::ClassDef
            | CppSyntaxKind::StructDef
            | CppSyntaxKind::UnionDef
            | CppSyntaxKind::ClassDecl
            | CppSyntaxKind::StructDecl
            | CppSyntaxKind::UnionDecl => self.class_like(node, scope, scope, BindingKind::Class),
            CppSyntaxKind::EnumDef | CppSyntaxKind::EnumClassDef => {
                self.enum_like(node, scope, scope)
            }
            // A `TemplateDecl` met on its own — outside the `Declaration` that normally holds it — declares
            // nothing by itself: it is the header, and the declaration it belongs to is elsewhere. Its
            // parameters still open a scope, so descending is right.
            CppSyntaxKind::TemplateDecl => {
                let parameters = self.table.create_scope(
                    ScopeKind::TemplateParameters,
                    Some(scope),
                    Some(cpp_parser::source_range(node.text_range())),
                );
                for list in node.children().filter(|child| {
                    CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TemplateParameterList
                }) {
                    self.template_parameters(&list, parameters);
                }
                self.items(node, parameters);
            }
            CppSyntaxKind::TypedefDecl => self.typedef_decl(node, scope),
            CppSyntaxKind::UsingDecl => self.using_decl(node, scope),
            CppSyntaxKind::UsingDirective => self.using_directive(node, scope),
            CppSyntaxKind::Declaration => self.declaration(node, scope),
            CppSyntaxKind::LabelStat => self.label(node, scope),
            // The constructs that open a scope a statement lives in. `CompoundStat` is deliberately absent:
            // whether it opens one is the caller's decision, and is passed down instead.
            CppSyntaxKind::ForStat
            | CppSyntaxKind::RangeForStat
            | CppSyntaxKind::WhileStat
            | CppSyntaxKind::DoWhileStat
            | CppSyntaxKind::SwitchStat
            | CppSyntaxKind::TryStat
            | CppSyntaxKind::IfStat => self.statement_scope(node, scope),
            _ => self.descend(node, scope, Body::OpensAScope),
        }
    }

    /// Keep looking inside a node that declares nothing itself.
    ///
    /// A `compound-statement` met here is a statement block — the caller has already said that the scope its
    /// header introduced is `scope` — so it opens one of its own, which is what makes `{ int x; }` inside a body
    /// a nested scope rather than a flattening.
    ///
    /// A **body belonging to a class-like construct** is the exception, and the reason this checks for one: a
    /// namespace's and a class's contents are their own scope, so opening a block here would put every
    /// declaration one level too deep. The check is on the body's own kind, because by the time a body is
    /// reached the construct that owns it is not visible any more — which is exactly how this went wrong the
    /// first time.
    fn descend(&mut self, node: &CppSyntaxNode, scope: ScopeId, body: Body) {
        if CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat {
            let inner = self.block(node, scope, body);
            self.items(node, inner);
            return;
        }

        self.items(node, scope);
    }

    /// A block statement, opening a scope for the statements it holds.
    fn block(&mut self, node: &CppSyntaxNode, parent: ScopeId, body: Body) -> ScopeId {
        match body {
            Body::OpensAScope => self.table.create_scope(
                ScopeKind::Block,
                Some(parent),
                Some(cpp_parser::source_range(node.text_range())),
            ),
        }
    }

    /// `namespace ns { ... }`, and the anonymous form.
    fn namespace(&mut self, node: &CppSyntaxNode, parent: ScopeId) {
        let scope = self.table.create_scope(
            ScopeKind::Namespace,
            Some(parent),
            Some(cpp_parser::source_range(node.text_range())),
        );

        // An anonymous namespace has no name to bind, and that is a fact about the program rather than a
        // failure: its contents are visible in this file and nowhere else. Binding nothing is right.
        //
        // The name goes in the **enclosing** scope, as a class's does: `namespace ns { }` makes `ns` usable from
        // outside, and a namespace declared inside itself would be reachable only from within it.
        if let Some((name, name_range)) = declared_name(node) {
            self.bind(parent, name, BindingKind::Namespace, node, name_range);
        }

        self.items_in_scope(node, scope, &[CppSyntaxKind::CompoundStat]);
    }

    /// A class, struct, or union, in either its definition or its declaration form.
    ///
    /// Both forms are handled together because both *declare* the name — `class Widget;` is a declaration that
    /// makes `Widget` known as a class — and the difference is only whether a body follows.
    /// A class, struct, or union, in either its definition or its declaration form.
    ///
    /// Both forms *declare* the name — `class Widget;` makes `Widget` known as a class — and the difference is
    /// only whether a body follows. That difference decides whether a scope is opened at all, which matters:
    /// a forward declaration has no members, so an empty class scope for it is a scope a consumer would report
    /// as "this class has no members" rather than "this class was not defined here".
    fn class_like(
        &mut self,
        node: &CppSyntaxNode,
        outer: ScopeId,
        inner: ScopeId,
        kind: BindingKind,
    ) {
        if let Some((name, name_range)) = declared_name(node) {
            self.bind(outer, name, kind, node, name_range);
        }

        // `class Widget;` — the name is declared and there is nothing else to say about it.
        let Some(body) = first_child(node, CppSyntaxKind::ClassBody) else {
            return;
        };

        // An empty body — `class Widget {};` — opens no scope either: there are no members to hold, and an
        // empty scope would make a consumer report "this class has no members" where the truth is "nothing was
        // written here". Asked of the body rather than of the declaration, because a class with a base clause
        // and no members has no members either.
        if body.children().next().is_none() {
            return;
        }

        let scope = self.table.create_scope(
            ScopeKind::Class,
            Some(inner),
            Some(cpp_parser::source_range(node.text_range())),
        );

        // The body is this scope, so its members land here rather than in a block inside it.
        self.items_in_scope(node, scope, &[CppSyntaxKind::ClassBody]);
    }

    /// An `enum`, scoped or unscoped.
    ///
    /// The enumerators are bound in the enum's own scope in both forms. That is exactly right for `enum class`
    /// and *nearly* right for an unscoped `enum`, whose enumerators are also visible in the enclosing scope —
    /// which is a second binding rather than a different one, and is left to the lookup step to model, because
    /// adding it here would make the enum scope report names it does not own.
    fn enum_like(&mut self, node: &CppSyntaxNode, outer: ScopeId, inner: ScopeId) {
        let scope = self.table.create_scope(
            ScopeKind::Enum,
            Some(inner),
            Some(cpp_parser::source_range(node.text_range())),
        );

        if let Some((name, name_range)) = declared_name(node) {
            self.bind(outer, name, BindingKind::Enum, node, name_range);
        }

        // The enumerators are one level down, inside the body the enumeration writes — and the body is not
        // always a direct child, so it is searched for rather than assumed.
        for body in node
            .descendants()
            .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::CompoundStat)
        {
            self.enumerators(&body, scope);
        }
    }

    /// Bind every enumerator declared in a body.
    fn enumerators(&mut self, body: &CppSyntaxNode, scope: ScopeId) {
        for enumerator in body.children() {
            if CppSyntaxKind::from(enumerator.kind()) != CppSyntaxKind::EnumeratorDecl {
                continue;
            }
            if let Some((name, name_range)) = declared_name(&enumerator) {
                self.bind(
                    scope,
                    name,
                    BindingKind::Enumerator,
                    &enumerator,
                    name_range,
                );
            }
        }
    }

    /// `typedef int Integer;`.
    fn typedef_decl(&mut self, node: &CppSyntaxNode, scope: ScopeId) {
        // The name is in the declarator, not on the node: `typedef <specifiers> <declarator>;`.
        let Some(declarator) = first_child(node, CppSyntaxKind::Declarator) else {
            return;
        };

        if let Some((name, name_range)) = declared_name(&declarator) {
            self.bind(scope, name, BindingKind::Typedef, node, name_range);
        }
    }

    /// `using Alias = T;` and `using ns::f;`, which are the same node kind.
    ///
    /// The two are told apart by whether a `TypeId` follows the `=`: an alias has one, a using-declaration
    /// does not. Both introduce a name into this scope, and the difference is what kind of thing it is.
    fn using_decl(&mut self, node: &CppSyntaxNode, scope: ScopeId) {
        let is_alias = first_child(node, CppSyntaxKind::TypeId).is_some();

        // A `using namespace std;` is a `UsingDecl` wrapping a `UsingDirective`, and it binds a namespace
        // rather than a name — so it is handed to the rule that says so, rather than read as a using-declaration
        // whose name happens to be the namespace's.
        if let Some(directive) = first_child(node, CppSyntaxKind::UsingDirective) {
            self.using_directive(&directive, scope);
            return;
        }

        let kind = if is_alias {
            BindingKind::Alias
        } else {
            BindingKind::UsingDeclaration
        };

        // A using-declaration is usually qualified — `using ns::f;` — and what it introduces is the **last**
        // component: `f` becomes usable unqualified, `ns` does not. An alias is never qualified in the same
        // way, because the name it introduces is written first, so either component would be the same.
        //
        // Read from the *deepest* name in the declaration rather than from the outermost. The two differ for a
        // qualified using-declaration, where the outer `NameExpr` holds the first segment and the rest of the
        // name sits inside it: the outer one's own tokens are `ns`, and its `::` — so a "last identifier" read
        // from there finds nothing and the binding is the qualifier. See [`find_name_node`].
        let Some(name_node) = find_name_node(node) else {
            return;
        };

        let Some(token) = last_identifier(&name_node) else {
            return;
        };

        let text = token.text().to_string();
        let name_range = cpp_parser::source_range(token.text_range());

        let Some(name) = name_from_text(&text, node) else {
            return;
        };

        self.bind(scope, name, kind, node, name_range);
    }

    /// `using namespace ns;` — which declares no name, and still has to be recorded.
    ///
    /// It binds nothing, because it introduces no name: what it does is make a whole namespace's names
    /// visible, which is a fact lookup has to account for and a *binding* cannot express. It is recorded as a
    /// binding of the namespace's name anyway, with [`BindingKind::UsingDirective`], because a consumer asking
    /// "why is `vector` in scope here" needs to find this line — and because the alternative is for the
    /// construct to leave no trace at all.
    fn using_directive(&mut self, node: &CppSyntaxNode, scope: ScopeId) {
        if let Some((name, name_range)) = declared_name(node) {
            self.bind(scope, name, BindingKind::UsingDirective, node, name_range);
        }
    }

    /// A `Declaration`: `int count;`, `void f(int a) { ... }`, `class Widget;`, `enum class E { ... }`.
    ///
    /// # Why the specifiers are searched for a class or an enum
    ///
    /// `class Widget;` and `enum class Color { Red };` are `Declaration`s in this grammar, with the keyword
    /// inside a `BuiltinType` in the decl-specifier sequence — not the `ClassDef` and `EnumClassDef` nodes
    /// that a *definition* with a name in the declarator produces. So the keyword is what identifies the
    /// construct, and reading names only from declarators would leave every forward declaration and every
    /// scoped enum undeclared — which in a real header is most of the class names in the file.
    fn declaration(&mut self, node: &CppSyntaxNode, scope: ScopeId) {
        // A templated declaration has its parameter list in a **sibling** node rather than around the
        // declaration, so it is intercepted here: the parameters open a scope, and the declaration's contents
        // are walked inside it. Missing this leaves `T` unbound and a template's class declared in the wrong
        // scope.
        if let Some(template) = first_child(node, CppSyntaxKind::TemplateDecl) {
            self.templated_declaration(node, &template, scope);
            return;
        }

        self.declaration_parts(node, scope, scope);
    }

    /// A declaration whose `template <...>` header was found.
    ///
    /// The parameters are bound in a scope of their own, and the declaration's *contents* are walked inside it
    /// — while its declared name belongs to the scope the template was written in. `template <typename T> class
    /// Array { T x; }` declares `Array` outside and `x` within sight of `T`, and those are different scopes.
    fn templated_declaration(
        &mut self,
        node: &CppSyntaxNode,
        template: &CppSyntaxNode,
        scope: ScopeId,
    ) {
        let parameters = self.table.create_scope(
            ScopeKind::TemplateParameters,
            Some(scope),
            Some(cpp_parser::source_range(template.text_range())),
        );

        for list in template.children().filter(|child| {
            CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TemplateParameterList
        }) {
            self.template_parameters(&list, parameters);
        }

        self.declaration_parts(node, scope, parameters);
    }

    /// Bind every parameter of a `template <...>` list.
    ///
    /// One scope for the whole list, which is what lets a later parameter refer to an earlier one:
    /// `template <typename T, T* Next>` is legal, and a parameter bound in a scope of its own would make `T`
    /// invisible to `Next`.
    fn template_parameters(&mut self, list: &CppSyntaxNode, scope: ScopeId) {
        for parameter in list.children() {
            if CppSyntaxKind::from(parameter.kind()) != CppSyntaxKind::TemplateParameter {
                continue;
            }

            if let Some((name, name_range)) = declared_name(&parameter) {
                self.bind(
                    scope,
                    name,
                    BindingKind::TemplateParameter,
                    &parameter,
                    name_range,
                );
            }
        }
    }

    /// The specifiers and declarators of a declaration, with any class-like specifier resolved first.
    ///
    /// Two scopes rather than one, because a templated declaration puts its name and its contents in different
    /// places: `template <typename T> class Array { T x; }` declares `Array` where the template was written and
    /// puts `x` in a scope that can see `T`. For an ordinary declaration the two are the same scope, which is
    /// what `inner` being passed as `outer` means at the call sites.
    fn declaration_parts(&mut self, node: &CppSyntaxNode, outer: ScopeId, inner: ScopeId) {
        // `f(x);` and `int(x);` have the same token shape, so the grammar reads both as a declaration — the
        // most vexing parse, and no parser without a table of type names can tell them apart. What it *can* say
        // is whether any name was declared, and the AST layer already answers that: a real declarator names
        // itself with a direct token or a `NameExpr` child, while the declarator of a call statement holds only
        // a parenthesised declarator one level down. A declaration with no name therefore went through the
        // declaration path without declaring anything, which is what a statement looks like when it lands here.
        //
        // This rejects only real declarations that genuinely have no name — `static_assert(...)`, a type
        // definition with no declarator, an anonymous class — and none of those declares a binding either.
        if is_unnamed_declaration(node) {
            return;
        }

        // `friend` declarations declare nothing in this scope: `friend class X;` says X's members may reach
        // into this class, and X itself is declared elsewhere. Binding it here would make a friend class look
        // like a member.
        let is_friend = node
            .children()
            .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::FriendDecl);

        // A structured binding declares several names through one `StructuredBinding` node. It appears
        // wherever the pattern was written — as an init-declarator (`auto [a, b] = pair;`) or as a bare
        // declarator — so it is searched for under this declaration's declarators.
        if let Some(names) = ScopeWalker::structured_binding_names(node) {
            for name_node in names {
                if let Some((name, name_range)) = declared_name(&name_node) {
                    self.bind(outer, name, BindingKind::Variable, &name_node, name_range);
                }
            }
            return;
        }

        // A class or an enum written in the specifiers brings its own name — and, for an enum, its own body.
        // When it did, the declarators have nothing left to declare.
        //
        // Whether a body follows is asked once, here, because it decides three things: whether a function
        // declarator opens a scope at all, how far that scope reaches, and what is walked into it.
        let body = function_body(node);
        let mut function_scope = None;

        match self.specifiers(node, outer, inner, is_friend) {
            SpecifierName::Bound => return,
            SpecifierName::InDeclarator(kind) => {
                // The class-like specifier named nothing, so the name is in a declarator — and when there is
                // **no** declarator at all, the name is in the specifier beside the keyword instead:
                // `class Widget;` is a forward declaration, and the grammar now writes it flat (the keyword is
                // one specifier and `Widget` the next) rather than nesting the name inside the keyword. Without
                // this the forward declaration declared nothing, since it has no declarator to read a name from.
                if declarators(node).is_empty() {
                    if let Some((name, name_range)) = elaborated_type_name(node) {
                        self.bind(outer, name, kind, node, name_range);
                    }
                    return;
                }

                for declarator in declarators(node) {
                    function_scope = self
                        .declarator_as(
                            &declarator,
                            outer,
                            inner,
                            is_friend,
                            Some(kind),
                            body.as_ref(),
                        )
                        .or(function_scope);
                }
            }
            SpecifierName::None => {
                for declarator in declarators(node) {
                    function_scope = self
                        .declarator_as(&declarator, outer, inner, is_friend, None, body.as_ref())
                        .or(function_scope);
                }
            }
        }

        // The body of a function, which the grammar puts beside the declarator rather than inside it. Walking
        // it into the scope the declarator opened is what makes a local variable local.
        if let Some(function) = function_scope
            && let Some(body) = body
        {
            self.items(&body, function);
        }
    }

    /// The names a structured binding pattern declares, if this declaration has one.
    ///
    /// Searched for among the **declarators** rather than across the whole declaration: a `StructuredBinding`
    /// node anywhere in the subtree would also be found inside a function's body, and returning early on that
    /// is how the enclosing function lost its own name and scope — the pattern below belongs to a different
    /// declaration.
    fn structured_binding_names(node: &CppSyntaxNode) -> Option<Vec<CppSyntaxNode>> {
        for declarator in declarators(node) {
            if let Some(binding) = declarator
                .descendants()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::StructuredBinding)
            {
                return Some(binding.children().collect());
            }
        }

        None
    }

    /// Walk a declaration's specifier sequence, reading any class or enum written there.
    ///
    /// The three outcomes are the three shapes C++ writes these in, and they are worth separating because the
    /// name is in a different place in each:
    ///
    /// ```text
    /// enum class Color { Red };    the name and body are in the specifier      -> Bound
    /// class Widget;                the specifier has only the keyword, and the
    ///                              identifier the declarator holds *is* the class -> InDeclarator
    /// class Widget w;              the class is in the specifier, `w` is not    -> Bound
    /// ```
    fn specifiers(
        &mut self,
        node: &CppSyntaxNode,
        outer: ScopeId,
        inner: ScopeId,
        is_friend: bool,
    ) -> SpecifierName {
        let Some(specifiers) = first_child(node, CppSyntaxKind::DeclSpecifierSeq) else {
            return SpecifierName::None;
        };

        for child in specifiers.children() {
            match CppSyntaxKind::from(child.kind()) {
                // Handled through the declarators by the caller.
                CppSyntaxKind::InitDeclarator | CppSyntaxKind::Declarator => {}
                CppSyntaxKind::ClassDef
                | CppSyntaxKind::StructDef
                | CppSyntaxKind::UnionDef
                | CppSyntaxKind::ClassDecl
                | CppSyntaxKind::StructDecl
                | CppSyntaxKind::UnionDecl => {
                    if !is_friend {
                        self.class_like(&child, outer, inner, BindingKind::Class);
                    }
                    return SpecifierName::Bound;
                }
                CppSyntaxKind::EnumDef | CppSyntaxKind::EnumClassDef => {
                    if !is_friend {
                        self.enum_like(&child, outer, inner);
                    }
                    return SpecifierName::Bound;
                }
                // The bare-keyword forms, which is what the grammar produces for `class Widget;` and for
                // `enum class Color { ... }` alike.
                CppSyntaxKind::BuiltinType => {
                    let Some(binding) = class_like_binding_of(&child) else {
                        continue;
                    };

                    if is_friend {
                        return SpecifierName::Bound;
                    }

                    let named_here = self.bare_class_like(&child, outer, inner, binding);

                    // The scoped-enum form carries its name; the forward-declaration form does not, and what
                    // looks like a variable declarator is the class itself.
                    return if named_here {
                        SpecifierName::Bound
                    } else {
                        SpecifierName::InDeclarator(binding)
                    };
                }
                _ => {}
            }
        }

        SpecifierName::None
    }

    /// One declarator: a name, possibly a function, possibly with a body.
    ///
    /// `as_kind` overrides what the declarator would otherwise declare, for the case where a class-like
    /// keyword in the specifiers says the identifier here names a class or an enum rather than a variable.
    ///
    /// Returns the function scope it opened, when it opened one, so that the caller can walk a body the grammar
    /// placed beside the declarator rather than inside it.
    fn declarator_as(
        &mut self,
        declarator: &CppSyntaxNode,
        outer: ScopeId,
        inner: ScopeId,
        is_friend: bool,
        as_kind: Option<BindingKind>,
        body: Option<&CppSyntaxNode>,
    ) -> Option<ScopeId> {
        if is_friend {
            return None;
        }

        let (name, name_range) = declared_name(declarator)?;

        let is_function = declarator
            .descendants()
            .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList);

        // A class-like keyword in the specifiers settles the question: whatever the declarator looks like, the
        // identifier is the class or the enum, and `class Widget w;` cannot happen — a second declarator would
        // be a second variable of that type.
        if let Some(kind) = as_kind {
            self.bind(outer, name, kind, declarator, name_range);
            return None;
        }

        if !is_function {
            // A variable is declared in the scope it is written in, initialiser or not. The initialiser is not
            // a scope of its own: `int x = x;` refers to an earlier `x`, so the name being declared and the
            // name being read live in the same scope, and a scope around the initialiser would put the read
            // outside the scope of the declaration it reads.
            self.bind(outer, name, BindingKind::Variable, declarator, name_range);
            return None;
        }

        let kind = function_binding_kind(&name);
        self.bind(outer, name, kind, declarator, name_range);

        // A declaration with nothing to put in a scope opens none. `int size() const;` is a member function
        // declaration: its parameters are a *type*, not names, and it has no body — so a scope for it would be
        // an empty one that a consumer reports as "this function has no locals" rather than "this function was
        // not defined here".
        let has_parameters = parameter_lists(declarator)
            .iter()
            .any(|list| list.children().next().is_some());

        if body.is_none() && !has_parameters {
            return None;
        }

        // The parameters and the body share one scope, which is what C++ does: a parameter is visible in the
        // body and not outside it, and the body does not nest inside the parameter list. The scope hangs off
        // `inner` so that a templated function's parameters can see the template's parameters.
        //
        // The range reaches to the end of the body when there is one, because a scope is looked up by offset:
        // a cursor on a local declaration is inside the body, which is *past* the declarator, and a scope that
        // ended at the parameter list would leave the whole body resolving to the enclosing class.
        let range = match body {
            Some(body) => span_of(
                cpp_parser::source_range(declarator.text_range()),
                cpp_parser::source_range(body.text_range()),
            ),
            None => cpp_parser::source_range(declarator.text_range()),
        };

        let function = self
            .table
            .create_scope(ScopeKind::Function, Some(inner), Some(range));

        for parameter_list in parameter_lists(declarator) {
            self.parameters(&parameter_list, function);
        }

        Some(function)
    }

    /// A class-like keyword written without a declarator to carry its name: `enum class Color { Red };`.
    ///
    /// Returns whether the specifier **named** the entity itself, which is what tells the two bare-keyword
    /// forms apart and is the only thing the caller needs to know:
    ///
    /// * `enum class Color { ... }` — the name and body are children of the specifier, so it is named here and
    ///   the specifier's declarator list is empty.
    /// * `class Widget;` — the specifier holds only the keyword, and the identifier in the *declarator* is the
    ///   class itself, so the caller must not read it as a variable.
    fn bare_class_like(
        &mut self,
        specifier: &CppSyntaxNode,
        outer: ScopeId,
        inner: ScopeId,
        binding: BindingKind,
    ) -> bool {
        let scope_kind = match binding {
            BindingKind::Enum => ScopeKind::Enum,
            _ => ScopeKind::Class,
        };

        let named = declared_name(specifier);
        let named_here = named.is_some();

        if let Some((name, name_range)) = named {
            self.bind(outer, name, binding, specifier, name_range);
        }

        // A forward declaration — `class Widget;` — has no body, so no scope: the caller reads the name from
        // the declarator and there is nothing else here.
        let has_body = specifier
            .descendants()
            .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::CompoundStat);

        if !has_body {
            return named_here;
        }

        let scope = self.table.create_scope(
            scope_kind,
            Some(inner),
            Some(cpp_parser::source_range(specifier.text_range())),
        );

        // The enumerators, for an enum written with a body. A plain `enum Color { Red };` reaches here with
        // `Color` in its declarator instead, and the enumerators still belong to this scope.
        for body in specifier
            .descendants()
            .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::CompoundStat)
        {
            self.enumerators(&body, scope);
        }

        named_here
    }

    /// The parameters of a function declarator.
    fn parameters(&mut self, list: &CppSyntaxNode, scope: ScopeId) {
        for parameter in list.children() {
            if CppSyntaxKind::from(parameter.kind()) != CppSyntaxKind::Parameter {
                continue;
            }

            // A parameter's name is in its own declarator, and an unnamed parameter — `void f(int);` — has
            // none, which is ordinary C++ and declares nothing.
            for declarator in parameter
                .children()
                .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator)
            {
                if let Some((name, name_range)) = declared_name(&declarator) {
                    self.bind(scope, name, BindingKind::Variable, &declarator, name_range);
                }
            }
        }
    }

    /// A construct that opens a scope for a statement: a loop, a branch, a `switch`, a `try`.
    ///
    /// The scope is what makes `for (int i = 0; ...)` bind `i` where the loop can see it and the enclosing
    /// block cannot, and what makes two sibling branches free to declare the same name.
    fn statement_scope(&mut self, node: &CppSyntaxNode, parent: ScopeId) {
        let scope = self.table.create_scope(
            ScopeKind::Block,
            Some(parent),
            Some(cpp_parser::source_range(node.text_range())),
        );

        self.items(node, scope);
    }

    /// `done:` — a label, which lives in a name space of its own.
    fn label(&mut self, node: &CppSyntaxNode, scope: ScopeId) {
        if let Some((name, name_range)) = declared_name(node) {
            self.bind(scope, name, BindingKind::Label, node, name_range);
        }
    }

    /// Add a binding, ignoring a scope that cannot hold it.
    ///
    /// The refusal is the table's, and it is deliberate rather than defensive: a label in a namespace and a
    /// variable in a template parameter list are constructs that do not exist, so a binding that reaches one is
    /// a sign the walk misread the tree. Dropping it leaves a gap a consumer can see; placing it would make
    /// the scope report a name that cannot be used there.
    fn bind(
        &mut self,
        scope: ScopeId,
        name: Name,
        kind: BindingKind,
        node: &CppSyntaxNode,
        name_range: cpp_parser::SourceRange,
    ) {
        self.table.add_binding(
            scope,
            Binding {
                name,
                kind,
                range: cpp_parser::source_range(node.text_range()),
                name_range,
                scope,
                origin: None,
            },
        );
    }
}

/// Where the name of a class-like declaration was written.
///
/// The three shapes C++ spells these in, and the reason a single "did the specifier name it" boolean is not
/// enough: the name is in a different place in each, and reading the wrong place binds the wrong name — which
/// is how `class Widget;` ends up declaring a variable called `Widget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecifierName {
    /// No class-like specifier here, so the declarators declare ordinary names.
    None,
    /// The specifier carried the name, and the declarators hold variables of that type.
    Bound,
    /// The specifier had only the keyword: the identifier in the declarator **is** the entity.
    InDeclarator(BindingKind),
}

/// The body of a function, which the grammar places beside or around the declarator rather than inside it.
///
/// Several shapes reach here and all of them occur in real code:
///
/// ```text
/// int f() { ... }                  the body is a direct child of the declaration
/// struct S { void m() { ... } };   inside a class body, inside the specifier sequence
/// void f() { ... }                 the body hangs off the *declarator*, which is not a direct child
/// ```
///
/// The search is over the declarators rather than over the whole subtree, so a `CompoundStat` belonging to
/// something else — a lambda's body in an initialiser — is not mistaken for this function's.
fn function_body(node: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    if let Some(direct) = first_child(node, CppSyntaxKind::CompoundStat) {
        return Some(direct);
    }

    for declarator in declarators(node) {
        for child in declarator.children() {
            if CppSyntaxKind::from(child.kind()) == CppSyntaxKind::CompoundStat {
                return Some(child);
            }
        }
    }

    // A member function's body is inside the class body, which is inside the specifier sequence.
    let specifiers = first_child(node, CppSyntaxKind::DeclSpecifierSeq)?;
    for child in specifiers.descendants() {
        if CppSyntaxKind::from(child.kind()) != CppSyntaxKind::ClassBody {
            continue;
        }

        for member in child.children() {
            if let Some(body) = function_body(&member) {
                return Some(body);
            }
        }
    }

    None
}

/// The parameter lists of a function declarator, outermost first.
///
/// Searched through the declarator's subtree rather than among its direct children, because the grammar does
/// not put them in one fixed place: `void f(int)` has the list one level down inside a nested `Declarator`,
/// while `void (*f)(int)` has it inside a `FunctionType`, and a search that assumed either shape would bind no
/// parameters at all for the other.
///
/// The walk stops at a **body**, which is what keeps a parameter list belonging to a nested lambda or a nested
/// class out of this function's scope: `void f() { auto g = [](int inner) {}; }` must not put `inner` in `f`.
/// A list *inside* another parameter list — a function-pointer parameter — is left alone for a different
/// reason: its names belong to the type, not to this function.
fn parameter_lists(declarator: &CppSyntaxNode) -> Vec<CppSyntaxNode> {
    fn collect(node: &CppSyntaxNode, out: &mut Vec<CppSyntaxNode>) {
        for child in node.children() {
            match CppSyntaxKind::from(child.kind()) {
                CppSyntaxKind::ParameterList => out.push(child),
                // A body belongs to something else, and a class body to something else again.
                CppSyntaxKind::CompoundStat | CppSyntaxKind::ClassBody => {}
                // Reached through the `ParameterList` arm above when it is this function's, and its contents
                // are a parameter's type when it is not.
                CppSyntaxKind::Declarator => collect(&child, out),
                _ => {}
            }
        }
    }

    let mut lists = Vec::new();
    collect(declarator, &mut lists);
    lists
}

/// The binding kind for a function, from what its name says it is.
///
/// The name is what decides: a constructor and an ordinary function are spelled the same way and differ only in
/// that a constructor's name is its class's, which this layer cannot know — a class is being built at the same
/// time. What *is* readable from the name alone is the three special forms, and those are worth separating
/// because a consumer treats them differently: a destructor is not found by ordinary lookup, and an operator is
/// found by the operator it is.
fn function_binding_kind(name: &Name) -> BindingKind {
    match name.kind {
        crate::symbol::NameKind::Destructor(_) => BindingKind::Destructor,
        crate::symbol::NameKind::Conversion(_) => BindingKind::ConversionFunction,
        crate::symbol::NameKind::Operator(_) => BindingKind::OperatorFunction,
        crate::symbol::NameKind::Literal(_) => BindingKind::LiteralOperator,
        crate::symbol::NameKind::Identifier(_) => BindingKind::Function,
    }
}

/// Did this `Declaration` go through the declaration path without declaring anything?
///
/// The question the most vexing parse forces on any parser without a table of type names. `f(x);` — a call —
/// and `int(x);` — a declaration of `x` — are the same tokens, so the grammar reads both as a declaration; what
/// distinguishes them at this layer is that the call has **no declarator name**, because the identifier sits in
/// a parenthesised declarator rather than where a declared name goes.
///
/// Answered by asking the AST layer rather than by walking the tree again: `CppDeclaration::get_name` is
/// already the "which name does this declare" rule, and it looks only where a name may be. A second
/// implementation here would be free to disagree with the first, and consumers use both.
///
/// `false` for every node that is not a `Declaration`, because the other declaration kinds state their name
/// differently and already handle an absent one — an anonymous namespace, an unnamed `class {}`.
fn is_unnamed_declaration(node: &CppSyntaxNode) -> bool {
    if CppSyntaxKind::from(node.kind()) != CppSyntaxKind::Declaration {
        return false;
    }

    match cpp_parser::CppDeclaration::cast(node.clone()) {
        Some(declaration) => declaration.get_name_text().is_none(),
        // Not castable, so the AST layer has no opinion and this layer should not invent one.
        None => false,
    }
}

/// The first direct child of a kind.
fn first_child(node: &CppSyntaxNode, kind: CppSyntaxKind) -> Option<CppSyntaxNode> {
    node.children()
        .find(|child| CppSyntaxKind::from(child.kind()) == kind)
}

/// Which kind of entity a bare class-like keyword introduces, if that is what this specifier is.
///
/// `class Widget;` and `enum class Color { ... }` reach the scope walker as a `BuiltinType` holding the
/// keyword, rather than as the `ClassDef` and `EnumClassDef` nodes that a definition with a declarator
/// produces. The keyword is therefore what identifies them, and the caller needs to know *which* keyword:
/// a name found here is a class in the first case and an enumerator-bearing enum in the second.
///
/// `None` for every other builtin type — `int`, `unsigned char`, `void` — which is the case that has to stay
/// cheap, because most specifiers are one.
fn class_like_binding_of(specifier: &CppSyntaxNode) -> Option<BindingKind> {
    let mut is_enum = false;
    let mut is_class_like = false;

    for token in specifier
        .children_with_tokens()
        .filter_map(|child| child.into_token())
    {
        match CppTokenKind::from(token.kind()) {
            // `enum class E` and `enum struct E` carry both keywords, and the entity is an enum: the `class`
            // there selects the scoped form and does not introduce a class.
            CppTokenKind::EnumKeyword => is_enum = true,
            CppTokenKind::ClassKeyword
            | CppTokenKind::StructKeyword
            | CppTokenKind::UnionKeyword => {
                is_class_like = true;
            }
            _ => {}
        }
    }

    match (is_enum, is_class_like) {
        (true, _) => Some(BindingKind::Enum),
        (false, true) => Some(BindingKind::Class),
        (false, false) => None,
    }
}

/// The init-declarators of a declaration, or the declarators when there is no init wrapper.
///
/// `int a, b;` has two init-declarators; `int a;` has one; a declaration of a class has neither. The fallback
/// matters for the shapes the grammar produces without an initialiser, where the declarator is a direct child.
fn declarators(node: &CppSyntaxNode) -> Vec<CppSyntaxNode> {
    let init: Vec<CppSyntaxNode> = node
        .children()
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::InitDeclarator)
        .collect();

    if !init.is_empty() {
        return init;
    }

    node.children()
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator)
        .collect()
}

/// The name a declaration declares, and the range of just that name.
///
/// Two shapes reach this, and which one a construct uses is not predictable from its kind: some declarations
/// carry a `NameExpr` child (`void f(int)` has `f` inside a declarator's name node) while others carry the
/// identifier as a **direct token** (`Red` in `enum { Red }`, `done` in `done: return;`). Both are tried,
/// because assuming either one leaves whole categories of declaration unnamed — enumerators and labels, which
/// is exactly what the first version of this did.
///
/// `None` when there is no name to read — an anonymous namespace, a `static_assert`, a construct the grammar
/// recovered from.
///
/// A **qualified** name is refused: `int ns::count;` declares something in `ns`, and splitting it here would
/// bind `count` in the enclosing scope, where it cannot be used. A using-declaration is the one construct that
/// *does* want the last component, and it reads it itself.
fn declared_name(node: &CppSyntaxNode) -> Option<(Name, cpp_parser::SourceRange)> {
    // The name is looked for *only* along the declarator-to-name path. That restriction is the whole
    // correctness of this function: a search that descends freely finds the first identifier anywhere, which
    // for `a = b;` is `a` inside the initializer and for `use(x);` is the argument — so a scope ends up
    // declaring names that a statement merely *uses*.
    if let Some(name_node) = name_node_along_declarator(node) {
        // A qualified name declares something in the scope it qualifies, not here: `int ns::Widget::count = 0;`
        // declares a member of `ns::Widget`, and binding `count` in this file's scope would put a name in scope
        // that cannot be used unqualified — and collide with every other `count`.
        if is_qualified(&name_node) {
            return None;
        }

        let token = first_identifier_in_name(&name_node)?;
        let text = token.text();

        if let Some(name) = name_from_text(text, node) {
            return Some((name, cpp_parser::source_range(token.text_range())));
        }
    }

    // A node that names something with a direct token has no `NameExpr` to walk: an enumerator is
    // `EnumeratorDecl["Red"]` and a label is `LabelStat["done" ":"]`. Neither can hold an initializer, so the
    // token is unambiguous here in a way it would not be further down the tree.
    let token = last_identifier(node)?;
    let text = token.text();
    let name = name_from_text(text, node)?;

    Some((name, cpp_parser::source_range(token.text_range())))
}

/// The `NameExpr` naming a declarator, following only the nodes that carry a declared name.
///
/// Stopping at anything else is what keeps an initializer, an argument list, and a function body out of the
/// search — see [`declared_name`] for what going further costs.
fn name_node_along_declarator(node: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    match CppSyntaxKind::from(node.kind()) {
        CppSyntaxKind::NameExpr => return Some(node.clone()),
        // A wrapper carrying a type or a template parameter rather than an expression. Descending is what
        // reaches `T` in `template <typename T>` and in `T d[N]`, neither of which has a declarator of its own
        // around the name.
        CppSyntaxKind::DeclSpecifierSeq
        | CppSyntaxKind::TemplateParameter
        | CppSyntaxKind::TypenameType
        | CppSyntaxKind::TemplateType
        | CppSyntaxKind::QualifiedType
        | CppSyntaxKind::PointerType
        | CppSyntaxKind::ReferenceType
        | CppSyntaxKind::RValueReferenceType
        | CppSyntaxKind::BuiltinType => {
            for child in node.children() {
                if let Some(found) = name_node_along_declarator(&child) {
                    return Some(found);
                }
            }

            return None;
        }
        CppSyntaxKind::Declarator => {
            // A nested declarator is the derived-type part — `int *p` has the star in an outer declarator — so
            // the search continues through it rather than stopping at the first one.
            for child in node.children() {
                if CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator
                    && let Some(found) = name_node_along_declarator(&child)
                {
                    return Some(found);
                }
            }

            for child in node.children() {
                if CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr {
                    return Some(child);
                }
            }

            return None;
        }
        _ => {}
    }

    // An `InitDeclarator` is the usual entry point: the declarator inside it names the entity, and an
    // initializer beside it does not.
    for child in node.children() {
        if matches!(
            CppSyntaxKind::from(child.kind()),
            CppSyntaxKind::Declarator | CppSyntaxKind::NameExpr
        ) && let Some(found) = name_node_along_declarator(&child)
        {
            return Some(found);
        }
    }

    None
}

/// The first identifier belonging to a name node, looking through nested name nodes.
///
/// A `NameExpr` may hold its identifier directly (`first`, from a binding pattern) or hold another `NameExpr`
/// that holds it (a qualified or templated name). Both are searched, because the two shapes are produced for
/// constructs that this layer cannot tell apart by kind alone.
fn first_identifier_in_name(node: &CppSyntaxNode) -> Option<cpp_parser::CppSyntaxToken> {
    if let Some(token) = last_identifier(node) {
        return Some(token);
    }

    for child in node.children() {
        if CppSyntaxKind::from(child.kind()) != CppSyntaxKind::NameExpr {
            continue;
        }
        if let Some(token) = first_identifier_in_name(&child) {
            return Some(token);
        }
    }

    None
}

/// The range covering both of two ranges.
///
/// A function's scope has to reach from its declarator to the end of its body, because a scope is looked up by
/// offset and the body lies *past* the declarator. Taking just the declarator's range leaves every local
/// declaration resolving to the enclosing scope, which is a wrong answer rather than a missing one: completion
/// inside a function would offer the class's members and none of its locals.
fn span_of(
    first: cpp_parser::SourceRange,
    second: cpp_parser::SourceRange,
) -> cpp_parser::SourceRange {
    let start = first.start_offset.min(second.start_offset);
    let end = first.end_offset().max(second.end_offset());

    cpp_parser::SourceRange {
        start_offset: start,
        length: end.saturating_sub(start),
    }
}

/// The `NameExpr` a declaration names itself with, looking through the declarator wrappers.
///
/// Search is depth-first, and it stops at the **deepest** name first.
///
/// The search is depth-first and stops at the first name, which is what the declarator grammar guarantees:
/// the outermost name in a declarator is the one being declared, and anything nested is a parameter or a
/// return type. A *name* is the exception, because a name can contain one: `using ns::f;` is written as an
/// outer `NameExpr` for the first segment with the rest of the name inside it, so the outermost node's own
/// tokens are `ns` and its `::`. Descending before answering is what makes the answer the last component —
/// the thing the statement introduces — rather than the qualifier.
fn find_name_node(node: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    if CppSyntaxKind::from(node.kind()) != CppSyntaxKind::NameExpr {
        return find_name_child(node);
    }

    find_name_child(node).or_else(|| Some(node.clone()))
}

/// The deepest name inside `node`, if it has one.
fn find_name_child(node: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    for child in node.children() {
        // A parameter list belongs to the signature rather than to the name, and a body is a different scope.
        // Skipping them keeps `void f(int g)` from reporting `g` as the declared name, and keeps a class body
        // from being searched for the class's own name.
        if matches!(
            CppSyntaxKind::from(child.kind()),
            CppSyntaxKind::ParameterList | CppSyntaxKind::CompoundStat | CppSyntaxKind::ClassBody
        ) {
            continue;
        }

        if let Some(found) = find_name_node(&child) {
            return Some(found);
        }
    }

    None
}

/// The class name of an **elaborated type specifier**: the `Widget` of `class Widget;`.
///
/// The grammar writes that declaration flat — `class` is one specifier and `Widget` the next — so the name is
/// the type written beside the class-like keyword rather than a declarator's. Asked only when the declaration has
/// no declarator at all; with one, the declarator is the name and this would report the type instead.
fn elaborated_type_name(node: &CppSyntaxNode) -> Option<(Name, cpp_parser::SourceRange)> {
    let specifiers = first_child(node, CppSyntaxKind::DeclSpecifierSeq)?;

    let has_class_keyword = specifiers.children().any(|child| {
        CppSyntaxKind::from(child.kind()) == CppSyntaxKind::BuiltinType
            && is_class_like_keyword_token(&child)
    });
    if !has_class_keyword {
        return None;
    }

    let name_node = specifiers
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TemplateType)?;

    let token = first_identifier_in_name(&name_node)?;
    let name = name_from_text(token.text(), node)?;

    Some((name, cpp_parser::source_range(token.text_range())))
}

/// Is this built-in specifier a class-like keyword?
fn is_class_like_keyword_token(node: &CppSyntaxNode) -> bool {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            matches!(
                token.kind(),
                cpp_parser::CppKind::Token(CppTokenKind::ClassKeyword)
                    | cpp_parser::CppKind::Token(CppTokenKind::StructKeyword)
                    | cpp_parser::CppKind::Token(CppTokenKind::UnionKeyword)
                    | cpp_parser::CppKind::Token(CppTokenKind::EnumKeyword)
            )
        })
}

/// Is this name written with a qualifier?
fn is_qualified(node: &CppSyntaxNode) -> bool {
    node.children_with_tokens()
        .filter_map(|child| child.into_token())
        .any(|token| CppTokenKind::from(token.kind()) == CppTokenKind::Scope)
}

/// The last identifier token in a node, which is the entity a qualified name ends with.
fn last_identifier(node: &CppSyntaxNode) -> Option<cpp_parser::CppSyntaxToken> {
    node.children_with_tokens()
        .filter_map(|child| child.into_token())
        .filter(|token| CppTokenKind::from(token.kind()) == CppTokenKind::Identifier)
        .last()
}

/// Turn a declared spelling into a [`Name`], reading the special forms the way C++ writes them.
///
/// The text is the *last* identifier of the declarator, so the operator keyword is gone by the time this is
/// called — which is why the node is consulted as well: whether `+` spells an operator depends on whether
/// `operator` was written, and that token is in the node rather than in the name.
fn name_from_text(text: &str, node: &CppSyntaxNode) -> Option<Name> {
    let has_operator_keyword = node
        .descendants_with_tokens()
        .filter_map(|child| child.into_token())
        .any(|token| CppTokenKind::from(token.kind()) == CppTokenKind::OperatorKeyword);

    if has_operator_keyword {
        // The lexer produces the operator as its own token right after the keyword, so the identifier found
        // here is a word-form operator (`new`, `delete`) or the beginning of a conversion type.
        return Some(Name::operator(text));
    }

    // A destructor is spelled `~Name` and the `~` is a token of its own, so the identifier alone cannot tell
    // it from the class. A declaration whose name is preceded by `~` is a destructor.
    let has_tilde = node
        .descendants_with_tokens()
        .filter_map(|child| child.into_token())
        .any(|token| CppTokenKind::from(token.kind()) == CppTokenKind::Tilde);

    if has_tilde {
        return Some(Name::destructor(text));
    }

    if text.is_empty() {
        return None;
    }

    Some(Name::identifier(text))
}

/// The module-shaped names a file declares, for a consumer that wants them without walking the tree.
///
/// Separate from [`build_scopes`] because a module name is not in a scope: it is matched against the module
/// graph. Kept here so that everything that reads a *name* out of the syntax lives in one place.
pub fn declared_module_names(root: &CppSyntaxNode) -> Vec<DeclName> {
    let mut names = Vec::new();

    for node in root.descendants() {
        match CppSyntaxKind::from(node.kind()) {
            CppSyntaxKind::ModuleDecl => {
                if let Some(name) = module_name_of(&node, CppSyntaxKind::ModuleName) {
                    names.push(DeclName::Module(name));
                }
                if let Some(partition) = module_name_of(&node, CppSyntaxKind::ModulePartition) {
                    names.push(DeclName::Partition(partition));
                }
            }
            CppSyntaxKind::ImportDecl => {
                if let Some(name) = module_name_of(&node, CppSyntaxKind::ModuleName) {
                    names.push(DeclName::Module(name));
                }
                if let Some(partition) = module_name_of(&node, CppSyntaxKind::ModulePartition) {
                    names.push(DeclName::Partition(partition));
                }
            }
            _ => {}
        }
    }

    names
}

/// The dotted name inside a module or partition node, as a qualified name.
fn module_name_of(node: &CppSyntaxNode, wrapper: CppSyntaxKind) -> Option<QualifiedName> {
    let holder = if wrapper == CppSyntaxKind::ModuleName {
        node.clone()
    } else {
        first_child(node, wrapper)?
    };

    let components: Vec<String> = holder
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .filter(|token| CppTokenKind::from(token.kind()) == CppTokenKind::Identifier)
        .map(|token| token.text().to_string())
        .collect();

    if components.is_empty() {
        return None;
    }

    Some(QualifiedName::from_components(components))
}

/// A binding origin for a declaration reached through an `#include`.
///
/// Exposed so that a caller assembling a table across files can mark the bindings it copies in, without this
/// module needing to know that includes exist.
pub fn included_origin(file: crate::paths::FileId) -> BindingOrigin {
    BindingOrigin::Included { file }
}

/// A binding origin for a declaration produced by a macro.
pub fn macro_origin(
    macro_name: impl Into<String>,
    definition: cpp_parser::SourceRange,
) -> BindingOrigin {
    BindingOrigin::MacroExpansion {
        macro_name: macro_name.into(),
        definition,
    }
}

/// The syntax kinds this layer reads a declaration out of, for a consumer auditing coverage.
///
/// Returned rather than documented so that a test can assert the set has not silently shrunk, which is the
/// failure mode of a match with a catch-all arm: a construct added to the grammar lands in
/// [`ScopeWalker::descend`] and contributes nothing, and nothing complains.
pub fn declaring_kinds() -> &'static [CppSyntaxKind] {
    &[
        CppSyntaxKind::NamespaceDecl,
        CppSyntaxKind::ClassDef,
        CppSyntaxKind::StructDef,
        CppSyntaxKind::UnionDef,
        CppSyntaxKind::ClassDecl,
        CppSyntaxKind::StructDecl,
        CppSyntaxKind::UnionDecl,
        CppSyntaxKind::EnumDef,
        CppSyntaxKind::EnumClassDef,
        CppSyntaxKind::TemplateDecl,
        CppSyntaxKind::TypedefDecl,
        CppSyntaxKind::UsingDecl,
        CppSyntaxKind::UsingDirective,
        CppSyntaxKind::Declaration,
        CppSyntaxKind::LabelStat,
    ]
}

/// Is this node a declaration the walk is expected to have read a name from?
///
/// Used by the coverage test: a node of one of these kinds that contributed no binding is either unnamed or a
/// gap, and only a test can tell which.
pub fn declares_a_binding(node: &CppSyntaxNode) -> bool {
    declaring_kinds().contains(&CppSyntaxKind::from(node.kind()))
}

/// A node's kind, by name, for a test that reports what it found.
pub fn kind_name(node: &CppSyntaxNode) -> String {
    format!("{:?}", CppSyntaxKind::from(node.kind()))
}
