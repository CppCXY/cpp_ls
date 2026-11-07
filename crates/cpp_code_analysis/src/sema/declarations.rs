//! Building a file's **declaration facts**: what the index stores about what a file declares.
//!
//! This is the producer [`crate::summary`] was defined for. It reads a file's scopes and its preprocessor
//! state together, which is the join nothing else in this crate performs — [`crate::build_scopes`] never sees a
//! directive and [`crate::preprocess::preprocess`] never sees a declaration.
//!
//! # What a fact is, and what it deliberately is not
//!
//! A fact is what the file **says**: a name was declared here, with this spelling, in this scope, inside this
//! conditional region. It is not a resolution — no "this refers to that", no type, no overload set. That is
//! That is the first invariant, and the reason is invalidation: a `#define` changes the meaning of
//! every name below it in every file that includes it, so a stored *conclusion* would have to be recomputed
//! project-wide, while a stored *fact* goes stale exactly when its own file changes.
//!
//! # Why the scope path is the valuable half
//!
//! A name alone cannot be looked up. Two files may each declare `Widget`, and one file may declare `f` at file
//! scope and again as a member of a class — so a fact carries the **qualified** name of the scope it was
//! declared in, which is what turns a flat list of declarations into something a lookup can index. That name is
//! derived, not stored twice: [`ScopeTree::qualified_name_of`] joins the segments the scopes recorded, and it
//! answers for `namespace a::b {` and `namespace a { namespace b {` alike.
//!
//! # Guards are computed in one pass
//!
//! Every fact says which conditional region it was written in, and the obvious implementation — ask
//! `FilePreprocessing::guard_at` once per fact — is quadratic, because that method replays every directive
//! from the top of the file each time. So the facts are sorted by offset and swept once alongside the
//! directives, keeping a stack of the regions currently open. The result is the same answer for a fraction of
//! the work, and it is the reason this module rather than `summary` owns the walk.

use std::collections::{HashMap, HashSet};

use crate::preprocess::FilePreprocessing;
use crate::preprocess::directive::{Directive, DirectiveKind, SpannedDirective};
use crate::sema::symbol::{Binding, BindingKind, ScopeId, ScopeKind, ScopeTree};
use crate::summary::{
    Access, ConditionalRegion, DeclFact, DeclKind, FactGuard, GuardBranch, MacroFact, MacroKind,
    SummaryGuards,
};
use cpp_parser::CppSyntaxKind;

use cpp_parser::{CppSyntaxNode, SourceRange};

/// Build a file's declaration facts, and the guard regions they refer to.
///
/// The two are returned together because a [`FactGuard`] is an index into the region list: handing back facts
/// without the list they index would be handing back dangling indices.
///
/// Declarations come from the **scope tree**, which is the layer that already decided what a declaration
/// declares — the alternative would be a second walk of the syntax tree implementing the same rules, free to
/// disagree with the first about `class Widget;` or about a qualified name. The cost is stated rather than
/// hidden: a declaration the scope walker deliberately does not bind is absent here too, and that absence is
/// deliberate on both sides — see [`crate::scopes::declaring_kinds`], which exists so that the set of
/// constructs the walker does read cannot silently shrink.
///
/// `root` is needed for one field: the **type** a variable was declared with, which is a fact about the syntax
/// and not about the scope tree. Taking it from the root's text rather than from a node keeps the walker out of
/// this module's business — the binding already says where the declaration and its name are, and what lies
/// between them is the type as the file spells it.
///
/// `errors` is the parser's diagnostic list, and it answers one field: whether the declaration a fact was written
/// in is one of them. It is **passed in** rather than looked up because the diagnostics live on the tree and this
/// module is handed a node — and because a caller that forgot to pass them would otherwise get a summary whose
/// every declaration claims to be clean, which is exactly the kind of silent default
/// [`crate::DeclFact::clean`]'s documentation warns about. An empty slice means "this file has no diagnostics",
/// which is the truth for a file that parses cleanly and is what the tests that do not care about the field
/// pass.
pub fn build_facts(
    scopes: &ScopeTree,
    preprocessing: &FilePreprocessing,
    root: &CppSyntaxNode,
    errors: &[SourceRange],
) -> (Vec<DeclFact>, SummaryGuards) {
    let file = DeclarationFacts::new(scopes, preprocessing, root, errors);
    file.build()
}

/// The type a variable-like declaration was written with, as it is spelled before the name.
///
/// # Why it is read out of the text
///
/// A `DeclFact` is built from a [`Binding`], which knows the declaration's range and its name's range and nothing
/// about the syntax in between — so the type is whatever the file wrote there. That is a *feature* here: it is
/// the spelling a consumer has to resolve anyway, and copying it out of the text cannot disagree with the file
/// about how it was written.
///
/// # What it strips, and what it keeps
///
/// Declaration **specifiers** go — they are not part of a type's name, and a lookup by name is what this is for.
/// So `static const Widget` records `Widget`, and a member access through it finds `Widget`'s members rather
/// than looking for a class called `static`. What stays is anything that names or shapes a type, which includes
/// `unsigned`/`long`/`short` (the words *are* the type) and `struct` (an elaborated specifier that does name
/// one). A `*` or `&` written before the name stays too: it does not change which class the type names.
///
/// # Why it is public
///
/// The *query* layer needs the same answer without going through a summary: a member access on a variable declared
/// in the file being edited asks its question of the tree in front of it, not of a cache. One implementation, so
/// that the two cannot come to disagree about what `static const Widget` declares.
///
/// `None` for a declaration that declares no type: a class, a namespace, a function, an alias. See
/// [`DeclFact::type_of`].
pub fn declared_type_of(root: &CppSyntaxNode, binding: &Binding) -> Option<String> {
    declared_type_of_with(&DeclarationShapes::of(root), binding)
}

/// [`declared_type_of`] for a caller that has the file's shapes already — which is every caller with more than one
/// question to ask. See [`DeclarationShapes`] for what building them costs and why that is the right trade.
///
/// # What changed, and what it cost to learn
///
/// This used to assemble a spelling out of **text**: it took the specifier sequence's words, cut the name and the
/// initializer out of the declarator's own text, and joined what was left. It is the same idea as
/// [`crate::sema::types::type_of_declaration`] and it is wrong in ways that only show up on real headers, because
/// the two halves of a type are not a text concatenation:
///
/// ```text
/// const [[nodiscard]] constexpr size_type   a list of keywords stripped from text leaves the attributes
/// std::map<std::string, int>                a class question needs `std::map`, not the whole spelling
/// friend constexpr iter_difference_t        a specifier the keyword list did not know became the type
/// ```
///
/// So the reading moved to [`type_of_declaration`], which walks the syntax; what is left here is finding the two
/// nodes and handing them over. Measured on the MinGW standard-library closure (30 185 declarations,
/// `examples/member_probe.rs`): **11 malformed spellings before, 0 after**.
///
/// # Why the name's range is still handed over
///
/// Because a `Binding` records it, and the reader uses it to descend — while a declarator's last `NameExpr` may
/// belong to a parameter (`int f(int x)`) or a trailing return type. The reader prefers its own reading of the name
/// when it can find one, and this range is what it falls back to.
/// **The target of a `using X = Y;`**, read from the `TypeId` after the `=`.
///
/// `None` for anything that is not a `using` declaration the name is inside, which is what makes this safe to ask
/// first: the ordinary `typedef` path and every variable declaration fall through to the specifiers-and-declarator
/// reading.
///
/// The name is matched by **being inside the declaration** rather than by equality, because a `using` can declare
/// several names in one line only as a template, and the range a binding records is the name's own:
/// `template <class T> using Vec = vector<T>;` binds `Vec`, and the `TypeId` is what its type is.
/// **The type a `using` alias names**, asked of the shapes rather than of a walk from the root.
///
/// # Why the tree is not asked
///
/// It was — `root.descendants()`, filtered to the innermost `UsingDecl` holding the name — and this question is
/// asked **once per binding**, so one pass over the file became quadratic in its bindings. Measured on a real
/// project (138 files): indexing **1.93 s → 27.0 s**, of which the `type-of` stage alone reported **66.5 s**
/// (stage time sums every call; the calls overlap in wall time).
///
/// It is the same defect the note above [`declared_type_of_with`] records — `declarator_declaring` walking from
/// the root, measured then at 2.9 s → 24.9 s on 356 files — which is what [`DeclarationShapes`] exists to answer
/// in one pass. A `using` declaration **is** one of the shapes, and the `TypeId` written after its `=` is recorded
/// with it, so the same answer costs a walk up the ancestor chain.
fn using_alias_target(shapes: &DeclarationShapes, name: cpp_parser::SourceRange) -> Option<String> {
    // **The innermost `using` the name is inside**: `on_the_path` visits outermost first, so the last one that
    // holds the name is the one that declares it — which is what "the smallest such node" meant when this walked.
    let mut target = None;
    shapes.on_the_path(name.start_offset, |shape| {
        if shape.kind == CppSyntaxKind::UsingDecl
            && let Some(type_id) = &shape.type_id
        {
            let spelling = type_id.text().to_string().trim().to_string();
            if !spelling.is_empty() {
                target = Some(spelling);
            }
        }
    });
    target
}

/// Does this declarator's span reach the start of the name it is being read for?
///
/// The **start**, not the whole range, for the reason the types module gives: a declarator's span ends at its own
/// last child, and for `* p` that child is the `PointerType` — so the span is `(50, 52)` while the name is
/// `(52, 53)`, and a test for full containment refuses the very declarator it is looking for.
fn reaches_the_name(node: &CppSyntaxNode, name: cpp_parser::SourceRange) -> bool {
    let span = node.text_range();
    usize::from(span.start()) <= name.start_offset && usize::from(span.end()) >= name.start_offset
}

pub(crate) fn declared_type_of_with(
    shapes: &DeclarationShapes,
    binding: &Binding,
) -> Option<String> {
    // **The kinds that name a type.** A field and a parameter are `Variable` too — they are what a member access is
    // asked *from* — and so is an **alias**, which is the kind this list was missing: `typedef _Ty& reference;`
    // records nothing under a rule that only allows `Variable`, so every member a class declares with its own
    // typedef had no type at all. That is the standard library's ordinary style (`typedef size_t size_type;` and
    // then `size_type size();`), and the cost was double: a member's own `type_of` was empty *and* the walk that
    // resolves one member's type against another's had nothing to read.
    //
    // What is still left out is the two kinds whose spelling comes from a different part of the syntax: a class
    // declares no type *as a name* (it *is* one), and a function *returns* one — [`DeclFact::returns`] is that
    // field, and `auto make() -> Widget` is why the two cannot be merged.
    if !matches!(
        binding.kind,
        BindingKind::Variable | BindingKind::Typedef | BindingKind::Alias
    ) {
        return None;
    }

    // **The specifiers are the innermost on the path and the declarator is the outermost**, which is the same walk
    // read in two directions and needs its reason stated, because "the innermost" is right for one and wrong for the
    // other:
    //
    // * a *specifier sequence* is written **before** the name, so the last one on the path is the declaration's own
    //   — `Widget` in `void f(Widget p)`, not the `void` the path passed through first;
    // * a *declarator* is a chain of nested nodes around **one** name, and the outermost of them is the whole type:
    //   `typedef void (*F)(int)` nests `(*F)(int)`, `(*F)` and `*`, and only the outermost carries the parameter
    //   list. Keeping the last reaching one answers `void*` with the parameter list silently dropped; keeping the
    //   widest by span answers `void` for `void f(Widget* p)` and `Widget` for `Widget* p`, because a declarator's
    //   span is not required to cover the name it declares (`Widget* p`'s inner level spans `* `, two bytes short).
    //
    // The walk is outermost-first, so no arithmetic is needed: the **first** declarator that reaches the name is the
    // one that declares it. That is what [`declarator_declaring`] answers from the tree, and the two agree by
    // construction rather than by being written twice.
    //
    // **Why the tree is not asked directly.** It was, and it cost 8.6× on a real project: `declarator_declaring`
    // walks from the root, and this function runs once per **binding** — 30 185 declarations in the corpus — so the
    // walk turns one pass into a quadratic one. Measured: 2.9 s → 24.9 s indexing 356 files. The shape walk exists
    // to answer these questions in one pass, and it can answer this one.
    let mut specifiers = None;
    let mut declarator: Option<CppSyntaxNode> = None;
    // **Asked once, not once per shape.** This was inside the visitor below, so every shape on the path paid a
    // `names_a_parameter` scan: measured on `arm_neon.h`, that was **1.9 ms per `Variable` binding** — 8.4 s of an
    // 8.5 s summary build — while the answer depends only on the offset, not on the shape.
    let name_is_a_parameter = shapes.names_a_parameter(binding.name_range.start_offset);
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        if let Some(node) = &shape.specifiers {
            specifiers = Some(node.clone());
        }
        // **The declarator that reaches the name — and for a parameter, the inner one.**
        //
        // A parameter's name is reached through *two* declarators: `int f(int op)` writes
        // `Declarator "f(int op)"` around `Declarator "op"`, and this walk sees the outer one first. Taking it read
        // the parameter as a **function declarator**, so `int f(Fn op)` produced a fact whose type was `Fn(Fn)` and
        // `int f(W op)` one whose type was `W(W)` — measured, for every named parameter of every function.
        //
        // What separates the two is **whether the name is inside a parameter list**: a parameter's is, and nothing
        // else's is. Not "the declarator holds a parameter list" — `typedef void (*F)(int)` holds one and its type is
        // the whole `void (*)(int)`, which the test `a_function_pointer_typedef_records_the_whole_type` pins. A
        // version that used the looser test broke exactly that, and this test is what said so.
        if declarator.is_none()
            && let Some(node) = &shape.declarator
            && reaches_the_name(node, binding.name_range)
            // **A direct child, not a subtree.** This asked `node.descendants()` — a walk of the whole declarator
            // subtree — for **every shape on the path**, and a function declarator is the biggest node on the path.
            // Measured on `arm_neon.h`: 6,703 bindings × 1.3 ms = **9.8 s of an 8.3 s summary build**, all of it here.
            // A parameter list is a *direct* child of the declarator that has one, which is the whole question.
            && !(name_is_a_parameter && is_a_function_declarator(node))
        {
            declarator = Some(node.clone());
        }
    });

    // **A `using` alias writes its type after the `=`**, in a `TypeId` of its own rather than in a declarator:
    // `using size_type = unsigned long;` has a `NameExpr` and a `TypeId` under the declaration and no specifier
    // sequence at all. So the two spellings of an alias take two branches — `typedef` reads specifiers plus
    // declarator, `using` reads the type id — and both end in the same field. Measured: without this branch a
    // `using` alias answered `void`, because the specifiers of the *enclosing* class were what the walk had last
    // seen.
    if let Some(target) = using_alias_target(shapes, binding.name_range) {
        return Some(target);
    }

    let specifiers = specifiers?;

    // The declared name is handed over, because an out-of-line definition writes it in the specifier sequence — see
    // [`crate::sema::types::type_of_declaration_named`]. **Except for a `typedef`**, whose specifier sequence is its
    // *type* and nothing else: `typedef typename _Base::reference reference;` writes the name twice, once inside the
    // type and once as the declared one, and the out-of-line rule (a qualified spelling that ends in the declared
    // name is the declarator's own name, not a type) then threw the **type** away and left nothing — which reads as
    // `void`. An alias is exactly the case where the two spellings must not be reconciled.
    //
    // **And except when the declarator itself holds the name.** That is what separates the two shapes the rule sees:
    //
    // ```text
    //   std::any any = 1;                        specifier `std::any`, declarator `any`      -> the name is the declarator's
    //   std::map<…> LuaLexer::LuaReserved = {…}; specifier `…::LuaReserved`, declarator `= {…}` -> the name is the specifier's
    // ```
    //
    // Both write a qualified spelling that ends with the declared name, and only the second is a definition of that
    // name. Asked without the distinction, `std::any any` recorded `type_of: Some("void")` — measured on
    // `containers.cpp` of the C++ corpus, where `assign-type-check` then reported the initialisation of an `std::any`
    // from `int` as a conversion "from `int` to `void`". The same shape is `Widget Widget;` or `S S;` in any file.
    let the_declarator_holds_the_name = declarator
        .as_ref()
        .is_some_and(|node| reaches_the_name(node, binding.name_range));
    let spelled = match binding.kind {
        BindingKind::Typedef => None,
        _ if the_declarator_holds_the_name => None,
        _ => Some(binding.name.text()),
    };
    let found = crate::sema::types::type_of_declaration_named(
        &specifiers,
        declarator.as_ref(),
        binding.name_range,
        spelled.as_deref(),
    );
    // **A variable never has a function type.** `std::unique_lock<std::mutex> lock(mutex, std::defer_lock);` is read
    // by the grammar as a *function declaration* — a value (`mutex`) is indistinguishable from an unnamed parameter's
    // type in the tokens — and the scope walker resolves that shape as an **initialisation** once it finds the value
    // (see `ScopeWalker::a_value_named`). What the object's type is, then, is what the declaration writes *before* the
    // parentheses, which is the function type's return half: measured, the fact said
    // `std::unique_lock<std::mutex>(mutex, std::defer_lock)` and a member access on `lock` resolved nothing.
    let found = match (&found, binding.kind) {
        (crate::sema::types::Type::Function { returns, .. }, BindingKind::Variable) => {
            returns.as_ref().clone()
        }
        _ => found,
    };

    let written = found.to_string();
    (!written.is_empty()).then_some(written)
}

/// **The modifiers a declaration is written with**, from the tokens the shape table already holds.
///
/// `override` and `final` are contextual identifiers, `[[maybe_unused]]` is an attribute and `virtual`/`constexpr` are
/// keywords inside the specifier sequence — so answering "does this declaration say `override`" used to mean slicing
/// the declaration's text at a check site and searching it. The bits are computed here, once, from the same shape
/// table the type and return-type questions use, and stored on the fact.
///
/// **The rendered text is not an argument any more**: `[[maybe_unused]]` was its one user, and it is answered from the attribute
/// node now — a child of the specifier sequence, or the declaration's previous sibling.
pub(crate) fn declared_modifiers_with(
    shapes: &DeclarationShapes,
    binding: &Binding,
) -> crate::DeclModifiers {
    // The declaration's own specifiers and declarator, selected exactly as [`declared_type_of_with`] selects them: the
    // innermost specifier sequence, and the first declarator that reaches the name (the inner one, for a parameter).
    let mut specifiers = None;
    let mut declarator: Option<CppSyntaxNode> = None;
    let name_is_a_parameter = shapes.names_a_parameter(the_offset_to_descend_by(binding));
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        if let Some(node) = &shape.specifiers {
            specifiers = Some(node.clone());
        }
        if declarator.is_none()
            && let Some(node) = &shape.declarator
            && reaches_the_name(node, binding.name_range)
            && !(name_is_a_parameter && is_a_function_declarator(node))
        {
            declarator = Some(node.clone());
        }
    });

    let mut found = crate::DeclModifiers::default();
    for node in [specifiers.as_ref(), declarator.as_ref()]
        .into_iter()
        .flatten()
    {
        found = found.union(modifiers_in(node));
    }

    // **The function's own qualifiers**, which are written *after* the parameter list: `void f() const &`,
    // `void g(int) &&`. They are neither in the specifier sequence nor inside the declarator, so the token walk above
    // cannot see them — it is given a node, and they are siblings that follow the declarator.
    if let Some(declarator) = declarator.as_ref() {
        found = found.union(function_suffix_in(declarator));
    }

    // **`= delete` and `= default` are written after the declarator**, in the init-declarator that holds it — neither in
    // the declarator itself nor in the specifiers. `modifiers_in` stops at bodies, so a lambda in an initialiser
    // (`auto f = [](int* p) { delete p; };`) is not this.
    //
    // Measured on libstdc++'s `make_unique`: its third overload is `= delete`d so that arrays are rejected, and without
    // the bit it tied with the real overload on every argument — `auto unique = std::make_unique<Resource>(1);` was
    // `Ambiguous` and the variable had no type.
    if let Some(init) = declarator
        .as_ref()
        .and_then(|declarator| declarator.parent())
    {
        found = found.union(modifiers_in(&init));
    }

    // **`[[maybe_unused]]`**, asked of the nodes: inside a class body the list is part of the specifier sequence, and at file
    // scope it is the declaration's previous sibling. Recorded as a bit because the checks read it far more often than the tree
    // is walked.
    if is_marked_maybe_unused(specifiers.as_ref(), declarator.as_ref()) {
        found = found.union(crate::DeclModifiers::MAYBE_UNUSED);
    }

    found
}

/// **The qualifiers written after a function's parameter list** — `const`, `volatile`, `&`, `&&`.
///
/// They are **the declarator's own trailing tokens**: the parser reads `void constant(int x) const {}` as an
/// `InitDeclarator` whose `Declarator` node's text is `constant(int x) const`. The scan therefore starts after the
/// declarator's `ParameterList` child, which is what keeps a `const` *inside* a parameter type (`void f(const int)`) and
/// a `&` in a return type (`int& f()`) out of it.
/// **The qualifiers written after a function's parameter list** — `const`, `volatile`, `&`, `&&`.
///
/// The parameter list's own **range** is the boundary: everything the declarator writes after it is its suffix, and the
/// qualifiers are token **kinds** rather than spellings. The version this replaces walked the tokens with an "after the
/// parameters" flag and matched `token.text()` against `"const"` — the same three answers, read from the tree's own shape.
fn function_suffix_in(declarator: &CppSyntaxNode) -> crate::DeclModifiers {
    use cpp_parser::{CppAstNode, CppDeclarator, CppTokenKind};
    let mut found = crate::DeclModifiers::default();
    let Some(declarator_node) = CppDeclarator::cast(declarator.clone()) else {
        return found;
    };
    let Some(parameter_list) = declarator_node.get_param_list() else {
        return found;
    };
    let after_the_parameters = parameter_list.get_range().end();
    for element in declarator_node.syntax().children_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        if token.text_range().start() < after_the_parameters {
            continue;
        }
        match CppTokenKind::from(token.kind()) {
            CppTokenKind::ConstKeyword => found = found.union(crate::DeclModifiers::CONST),
            CppTokenKind::VolatileKeyword => found = found.union(crate::DeclModifiers::VOLATILE),
            CppTokenKind::Ampersand => found = found.union(crate::DeclModifiers::LVALUE_REFERENCE),
            CppTokenKind::LogicalAnd => found = found.union(crate::DeclModifiers::RVALUE_REFERENCE),
            _ => {}
        }
    }
    found
}

/// **The `requires`-clause written after a function's parameter list**, as the declaration spells it.
///
/// The node is the declarator's **sibling**, not its child — `void constrained(int x) const requires (x > 0) {}` is an
/// `InitDeclarator` holding the declarator (with its `const`) and a `RequiresClause` after it. What is returned is the
/// clause's own text, keyword included (`requires (x > 0)`), because that is what the card prints after the signature;
/// `None` when the declaration writes none.
pub(crate) fn the_requires_clause_with(
    shapes: &DeclarationShapes,
    binding: &Binding,
) -> Option<String> {
    let mut declarator: Option<CppSyntaxNode> = None;
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        if let Some(node) = &shape.declarator
            && reaches_the_name(node, binding.name_range)
        {
            declarator = Some(node.clone());
        }
    });
    use cpp_parser::{CppAstNode, CppInitDeclarator};
    let parent = declarator.as_ref()?.parent()?;
    // **The clause is a sibling of the declarator**, in the init-declarator that holds both: `void f() const requires (x) {}`.
    let init = CppInitDeclarator::cast(parent)?;
    let clause = init.get_requires_clause()?;
    let text = clause.get_clause_text();
    (!text.is_empty()).then_some(text)
}

/// **Does this declaration say `[[maybe_unused]]`?**
///
/// The attribute is a node, in one of two places depending on where the declaration is written — a **child of the specifier
/// sequence** inside a class body, and a **previous sibling of the declaration** at file scope (both measured). The text scan
/// this replaces had to invent a statement boundary to keep a previous declaration's attribute out; a sibling walk cannot see
/// one, because it ends at the first sibling that is not an attribute list.
fn is_marked_maybe_unused(
    specifiers: Option<&CppSyntaxNode>,
    declarator: Option<&CppSyntaxNode>,
) -> bool {
    use cpp_parser::{CppAstNode, CppAttributeList, CppDeclSpecifierSeq};
    if specifiers
        .and_then(|node| CppDeclSpecifierSeq::cast(node.clone()))
        .is_some_and(|specifiers| specifiers.has_attribute("maybe_unused"))
    {
        return true;
    }
    let Some(declaration) = specifiers.and_then(|node| node.parent()).or_else(|| {
        declarator
            .and_then(|node| node.parent())
            .and_then(|node| node.parent())
    }) else {
        return false;
    };
    let mut before = cpp_parser::typed_previous_sibling::<CppAttributeList>(&declaration);
    while let Some(list) = before {
        if list.has_a_name("maybe_unused") {
            return true;
        }
        before = cpp_parser::typed_previous_sibling::<CppAttributeList>(list.syntax());
    }
    false
}

/// **Is this declarator a function's?** — the question that decides whether a name inside it is a parameter.
///
/// A parameter list is a **direct child** of the declarator that has one (measured; see the note at its use), so this is the
/// typed accessor rather than a search of the declarator's subtree.
fn is_a_function_declarator(declarator: &CppSyntaxNode) -> bool {
    use cpp_parser::CppAstNode;
    cpp_parser::CppDeclarator::cast(declarator.clone())
        .is_some_and(|declarator| declarator.get_param_list().is_some())
}

/// The bits one node's tokens state — the specifier sequence, or the declarator.
///
/// Tokens rather than a text search: `override` and `final` arrive as identifiers, `delete`/`default` as their
/// keywords, `...` as an ellipsis, and an attribute carries its word as an identifier too. The comparison is against a
/// token's own text, which is the spelling the lexer produced, not a slice of the file that could match a comment.
fn modifiers_in(node: &CppSyntaxNode) -> crate::DeclModifiers {
    use cpp_parser::CppAstNode;
    let mut found = crate::DeclModifiers::default();
    // **Bodies are not the declaration's own words.** A class's specifier sequence *contains its body*, so a
    // descendant scan answered "this class is `virtual`" because a member function was — measured by
    // `the_virt_specifiers_are_bits`. The walk stops at a `ClassBody` or a `CompoundStat` and keeps everything else:
    // specifier nodes, the declarator's suffix (`override`, `final`), a parameter list, an attribute.
    let mut stack: Vec<(CppSyntaxNode, bool)> = vec![(node.clone(), false)];
    while let Some((current, in_a_parameter_list)) = stack.pop() {
        for element in current.children_with_tokens() {
            if let Some(token) = element.as_token().cloned() {
                let word = token.text();
                let bit = match word {
                    "virtual" => Some(crate::DeclModifiers::VIRTUAL),
                    "override" => Some(crate::DeclModifiers::OVERRIDE),
                    "final" => Some(crate::DeclModifiers::FINAL),
                    "constexpr" => Some(crate::DeclModifiers::CONSTEXPR),
                    "consteval" => Some(crate::DeclModifiers::CONSTEVAL),
                    "constinit" => Some(crate::DeclModifiers::CONSTINIT),
                    "static" => Some(crate::DeclModifiers::STATIC),
                    "inline" => Some(crate::DeclModifiers::INLINE),
                    "explicit" => Some(crate::DeclModifiers::EXPLICIT),
                    "maybe_unused" => Some(crate::DeclModifiers::MAYBE_UNUSED),
                    "nodiscard" => Some(crate::DeclModifiers::NODISCARD),
                    "delete" => Some(crate::DeclModifiers::DELETED),
                    "default" => Some(crate::DeclModifiers::DEFAULTED),
                    _ => None,
                };
                if let Some(bit) = bit {
                    found = found.union(bit);
                }
                // **A variadic parameter list**, which is the `...` of a function rather than of a pack expansion
                // inside an expression: only a `...` inside a parameter list counts.
                if in_a_parameter_list
                    && cpp_parser::CppTokenKind::from(token.kind())
                        == cpp_parser::CppTokenKind::Ellipsis
                {
                    found = found.union(crate::DeclModifiers::VARIADIC);
                }
                continue;
            }
            let Some(child) = element.as_node().cloned() else {
                continue;
            };
            // **A body is not the declaration's own words**: a class's specifier sequence contains its body, so the walk stops
            // at a `ClassBody` or a `CompoundStat`. The parameter-list flag is what separates a variadic function's `...` from a
            // pack expansion in an expression.
            if cpp_parser::CppClassBody::can_cast(child.kind().into())
                || cpp_parser::CppCompoundStat::can_cast(child.kind().into())
            {
                continue;
            }
            let child_is_a_parameter_list = cpp_parser::CppParamList::can_cast(child.kind().into());
            stack.push((child, in_a_parameter_list || child_is_a_parameter_list));
        }
    }

    found
}
/// Which offset the walks in this module descend by: the **name**, not the binding's range.
///
/// A `Binding`'s range is "the declarator" for a variable declared in a statement, and that is what the walks
/// used to follow. It is not that for a **parameter**: `void f(Widget* p)` binds `p` over the whole parameter, so
/// a walk anchored on its start enters the *specifier* sequence and never meets the declarator — the `*` was
/// lost, and `(*p).size` answered "the type of `p` is not known here". The name is inside the declarator on every
/// declaration this is asked about, which is the property the walk actually needs.
fn the_offset_to_descend_by(binding: &Binding) -> usize {
    if binding.name_range.length > 0 {
        binding.name_range.start_offset
    } else {
        binding.range.start_offset
    }
}

/// **The declaration nodes of one file**, in the one form the four "what does this declaration say" questions need.
///
/// # Why this exists, in one measurement
///
/// [`declared_type_of`], [`declared_returns_of`], [`declared_alias_target`] and [`declared_bases_of`] all answered
/// by *descending from the root to the binding* and keeping what they passed on the way. Descending scans a node's
/// children until it finds the one holding the offset, so it is O(siblings) at every level and therefore
/// O(declarations in the file) **per binding**. On the 138-file closure of `<iostream>` + `<string>` that was
/// **14.4 s of a 23.8 s index** — 7.5 s in `declared_type_of`, 6.9 s in `declared_returns_of`, against 0.97 s for
/// *parsing* every one of those files. `crate::stages` is the instrument that found it; this type is the fix.
///
/// The same questions, asked of a file whose shapes have been read once: the node holding an offset is found by
/// binary search, and then the ancestor chain is walked **upward**, which is O(how deeply declarations nest).
///
/// # What a shape is
///
/// One node with something to say about a binding inside it: the children a question reads (`DeclSpecifierSeq`,
/// `TrailingReturnType`, `Declarator`, a `using`'s `TypeId`, a class's base names) or a kind that is a declaration
/// in its own right (`using`/`typedef`, and the class-like definitions whose bases are read). A node with none of
/// those is not a shape — no question in this module is answered by one.
pub struct DeclarationShapes {
    /// Every shape, in **document order** (pre-order) — which is also start-offset order, the order the binary
    /// search over a sibling run relies on.
    shapes: Vec<Shape>,
    /// The shapes with no parent: a run in `children`.
    roots: (u32, u32),
    /// Each shape's direct children, as runs — see [`DeclarationShapes::of`] for why they are laid out separately.
    children: Vec<u32>,
    /// **The class each template declaration introduces**, as `(the specifier sequence it introduces, its parameter
    /// list)`, in document order — see [`declared_template_parameters_of`] for why the question cannot be answered
    /// from the parent chain: `template <…>` and the class it introduces are *siblings*, and the specifier sequence
    /// between them is not itself a shape.
    templates: Vec<(cpp_parser::SourceRange, CppSyntaxNode)>,
    /// **Where the file's parameter lists are**, in document order (so start-offset order), so that
    /// [`DeclarationShapes::names_a_parameter`] is a binary search rather than a descent from the root.
    ///
    /// This replaces the whole tree, which was kept for that one question: `names_a_parameter` then did
    /// `token_at(root, offset)` — a descent that scans siblings at every level — once per binding. Measured on
    /// `arm_neon.h`: **2,032 ms of a 2,060 ms build**, on 6,703 bindings.
    ///
    /// **The tree these shapes were read from** — retained for the calls above that still read nodes out of it.s *ancestry*
    /// rather than about a shape: whether a name stands inside a parameter list — see
    /// [`DeclarationShapes::names_a_parameter`].
    parameter_lists: Vec<cpp_parser::SourceRange>,
}

/// One node a question is answered at, and how it is reached.
struct Shape {
    /// The node's own range — the interval that decides which bindings it is the shape *for*.
    at: cpp_parser::SourceRange,
    kind: CppSyntaxKind,
    /// **Who may name a declaration written here** — the access level in force in the class body this node is in,
    /// or `None` outside every class body. Recorded per shape rather than looked up per binding because the pass
    /// that builds this table is already walking the tree in document order, and the level is a property of *where*
    /// the declaration is written (see [`Access`], and [`declared_access_with`] for the query).
    access: Option<Access>,
    /// **Is a declaration written here exported** from the module this file declares — `export int f();`,
    /// `export namespace n { … }`, `export { … }`?
    ///
    /// A property of *where* it is written, like [`Shape::access`], and recorded in the same pass for the same
    /// reason. It decides whether an importer may name it: a declaration in a module interface unit that is not
    /// exported is invisible to `import`, which is the difference between a completion that compiles and one that
    /// does not — see `crate::ModuleReading` and the visibility walk that applies it.
    exported: bool,
    /// Its `DeclSpecifierSeq` child, if it has one.
    specifiers: Option<CppSyntaxNode>,
    /// Its `TrailingReturnType` child, if it has one.
    trailing: Option<CppSyntaxNode>,
    /// Its `Declarator` child, if it has one.
    declarator: Option<CppSyntaxNode>,
    /// A `using`'s `TypeId` child — the type the alias points at.
    type_id: Option<CppSyntaxNode>,
    /// A class-like definition's base **names** (the `NameExpr` inside each `BaseSpecifier`), in declaration
    /// order. The access keyword is deliberately not part of it: `public B` names `B`.
    bases: Vec<CppSyntaxNode>,
    /// **A class-like definition's written name, when that name carries a template argument list** — `atomic<_Ty>`,
    /// `vector<_Ty, _Alloc>` — and `None` for everything else, a plain `struct Widget` included.
    ///
    /// Recorded here rather than computed per declaration because it is a property of the **class**, and the class
    /// has many members: [`crate::DeclFact::pattern`] asks for it once per member, and answering it per member meant
    /// a descent from the root for every declaration in the file. Measured, that single call was **2245 ms of a
    /// 4562 ms `Facts` stage** over 126 files of MSVC's standard library — half of it — and most of the declarations
    /// it was asked about were not in a class at all, so the descent was spent discovering that.
    ///
    /// See [`written_class_pattern`] for what the spelling is and why it is read out of the head's text rather than
    /// out of the tree.
    pattern: Option<String>,
    /// The innermost shape containing this one — the chain [`DeclarationShapes::on_the_path`] walks.
    parent: Option<u32>,
    /// This shape's own children, as a run in [`DeclarationShapes::children`].
    children: (u32, u32),
}

impl DeclarationShapes {
    /// Read every shape of `root`, in one pass over the tree.
    ///
    /// The children are laid out as **runs** in a second array rather than as links, because the search that finds
    /// which child holds an offset needs random access to a parent's children, and in pre-order a parent's
    /// children are not consecutive: a child's whole subtree comes between it and its next sibling. Two counting
    /// passes put them in order — the order they were collected in, which is start-offset order, which is what the
    /// search assumes.
    pub fn of(root: &CppSyntaxNode) -> Self {
        use cpp_parser::CppAstNode;
        let mut shapes: Vec<Shape> = Vec::new();
        // The shapes the node being visited is inside, outermost first.
        let mut chain: Vec<u32> = Vec::new();
        // The class templates, recorded in the same pass — see the field's note.
        let mut templates: Vec<(cpp_parser::SourceRange, CppSyntaxNode)> = Vec::new();
        // The class bodies the walk has entered and the access level in force in each, innermost last — see
        // [`Shape::access`].
        let mut bodies: Vec<(usize, Access)> = Vec::new();
        // The `export`ed regions the walk has entered, innermost last — see [`Shape::exported`].
        let mut exports: Vec<usize> = Vec::new();

        let mut parameter_lists: Vec<cpp_parser::SourceRange> = Vec::new();
        for node in root.descendants() {
            let at = cpp_parser::source_range(node.text_range());
            // Recorded before any skip: a `ParameterList` is not a shape, and this is the only place that sees it.
            //
            // **Nested lists are folded into the outer one**, which makes the table disjoint and therefore a plain
            // binary search at query time. A pre-order walk sees a list before the lists nested inside it, so the last
            // entry is the only one a new list can be inside of.
            if cpp_parser::CppParamList::can_cast(node.kind().into()) {
                match parameter_lists.last_mut() {
                    Some(last)
                        if last.start_offset <= at.start_offset
                            && at.end_offset() <= last.end_offset() => {}
                    _ => parameter_lists.push(at),
                }
            }

            // Close every shape this node is not inside; what is left is the chain it belongs to.
            while chain
                .last()
                .is_some_and(|top| shapes[*top as usize].at.end_offset() <= at.start_offset)
            {
                chain.pop();
            }

            // …and the same for the **class bodies** the walk has entered: the access level is a property of the
            // region a declaration sits in, so a body that has ended cannot still be in force.
            while bodies
                .last()
                .is_some_and(|(end, _)| *end <= at.start_offset)
            {
                bodies.pop();
            }

            // …and for the **exported regions**. `export` heads three shapes and the grammar writes the keyword in
            // two different places — inside the declaration it exports and inside an export block, but *beside* a
            // namespace — so one function reads both spellings. See [`begins_an_exported_region`].
            while exports.last().is_some_and(|end| *end <= at.start_offset) {
                exports.pop();
            }
            if begins_an_exported_region(&node) {
                exports.push(at.end_offset());
            }

            let kind = CppSyntaxKind::from(node.kind());

            // **A class body opens an access region, and `public:`/`private:`/`protected:` change it.** The level in
            // force is what a consumer needs to decide whether a member may be *named* where the cursor is, and it
            // is a property of the text's own structure rather than of any resolution — which is why it is recorded
            // in this pass (one walk, no store) rather than asked per binding.
            //
            // The default before any label is the class key's: `class` is private, `struct` and `union` are public.
            // Read from the *parent* node, which is the class definition this body belongs to.
            if kind == CppSyntaxKind::ClassBody {
                // A `class` — and anything else this walk does not recognise — is private, which is the conservative
                // direction for a *member*: the private reading hides a name rather than offering one the reader cannot write.
                let default = node
                    .parent()
                    .and_then(cpp_parser::CppClassDef::cast)
                    .map_or(Access::Private, |class| {
                        if class.is_struct() || class.is_union() {
                            Access::Public
                        } else {
                            Access::Private
                        }
                    });
                bodies.push((at.end_offset(), default));
            } else if let Some((_, level)) = bodies.last_mut() {
                match kind {
                    CppSyntaxKind::PublicAccess => *level = Access::Public,
                    CppSyntaxKind::ProtectedAccess => *level = Access::Protected,
                    CppSyntaxKind::PrivateAccess => *level = Access::Private,
                    _ => {}
                }
            }

            // **The one relation that is not on the path to a name**, recorded while the pass is here anyway: a
            // template declaration and the class it introduces are siblings, so the pair is remembered by the range
            // the specifier sequence covers. Recorded *before* the shape test below, because a `TemplateDecl` is
            // not a shape — nothing in this module is answered at one.
            if kind == CppSyntaxKind::TemplateDecl
                && let Some(list) =
                    cpp_parser::typed_children::<cpp_parser::CppTemplateParamList>(&node).next()
                && let Some(owner) = node.parent()
            {
                // **The range is the declaration the template heads, not just its specifier sequence.**
                //
                // A class template's name is written in that sequence, so pairing the list with the sequence was
                // enough for `template <class T> struct S`. A **function** template's name is not there:
                // `template <class T> T h();` has the return type in the sequence and `h` in the declarator beside
                // it, which the reshape made a sibling of the template head. The pair was therefore recorded against
                // a range that never contains the name, `declared_template_parameters_with` found no template for it,
                // and a call that named its argument — `h<int>()`, or a deduced `_It` from a passed `int*` — kept the
                // **parameter's name** as the type instead of substituting it. Measured: two semantic tests.
                //
                // The outer of two nested templates is recorded with the outer declaration's range, which contains
                // the inner name as well — the "last entry in document order wins" rule that
                // `declared_template_parameters_with` already applies is what makes the innermost one the answer.
                templates.push((
                    cpp_parser::source_range(owner.text_range()),
                    list.syntax().clone(),
                ));
            }

            let read = children_a_question_reads(&node);
            if read.is_empty() && !declares_a_shape(kind) {
                continue;
            }

            let parent = chain.last().copied();
            // **The class's written pattern, read once for the class and not once per member.** See
            // [`Shape::pattern`].
            let pattern = match kind {
                CppSyntaxKind::ClassDef | CppSyntaxKind::StructDef | CppSyntaxKind::UnionDef => {
                    written_class_pattern(&node)
                }
                _ => None,
            };
            shapes.push(Shape {
                at,
                kind,
                pattern,
                access: bodies.last().map(|(_, level)| *level),
                exported: !exports.is_empty(),
                specifiers: read.specifiers,
                trailing: read.trailing,
                declarator: read.declarator,
                type_id: read.type_id,
                bases: read.bases,
                parent,
                children: (0, 0),
            });
            chain.push(shapes.len() as u32 - 1);
        }

        let mut runs = vec![(0u32, 0u32); shapes.len() + 1];
        for shape in &shapes {
            runs[slot_of(shape.parent)].1 += 1;
        }

        let mut placed = 0u32;
        let mut cursor = Vec::with_capacity(runs.len());
        for run in runs.iter_mut() {
            cursor.push(placed);
            run.0 = placed;
            placed += run.1;
        }

        let mut children = vec![0u32; placed as usize];
        for (index, shape) in shapes.iter().enumerate() {
            let slot = slot_of(shape.parent);
            children[cursor[slot] as usize] = index as u32;
            cursor[slot] += 1;
        }

        for (index, shape) in shapes.iter_mut().enumerate() {
            shape.children = runs[index + 1];
        }

        DeclarationShapes {
            shapes,
            roots: runs[0],
            children,
            templates,
            parameter_lists,
        }
    }

    /// **Does the name at this offset stand inside a parameter list?**
    ///
    /// The one question this type answers from the *tree* rather than from a shape, and it is asked because a
    /// parameter's name is reached through two declarators — `int f(int op)` writes `Declarator "f(int op)"` around
    /// `Declarator "op"` — so the shape walk sees the function's declarator first and read the parameter as a
    /// function. Measured: `int f(Fn op)` produced a fact whose type was `Fn(Fn)`.
    ///
    /// Answered by walking **up** from the token at the offset, which is what makes it about the name's place in the
    /// tree rather than about any node's contents: `typedef void (*F)(int)` also holds a parameter list, and its name
    /// is not inside it, so its type stays the whole `void (*)(int)`.
    pub fn names_a_parameter(&self, offset: usize) -> bool {
        // The lists are disjoint and in start order (nested ones were folded into their outer list when the table was
        // built), so the last one starting at or before the offset is the only candidate.
        let at = self
            .parameter_lists
            .partition_point(|range| range.start_offset <= offset);
        let Some(index) = at.checked_sub(1) else {
            return false;
        };
        offset < self.parameter_lists[index].end_offset()
    }

    /// How many shapes the file has — what a caller prints when it wants to know whether the walk was worth it.
    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    /// **Who may name a declaration written at this offset** — the access level in force there, or `None` outside
    /// every class body.
    ///
    /// The **innermost** shape containing the offset decides, which is the same direction every other query here
    /// reads: a member's own declarator sits inside its class body's region, and a nested class's members sit inside
    /// the nested body's — see [`Shape::access`] for why the level is recorded rather than looked up.
    pub fn access_at(&self, offset: usize) -> Option<Access> {
        let mut found = None;
        self.on_the_path(offset, |shape| found = shape.access);
        found
    }

    /// **The specifier tokens of the declaration this offset belongs to** — `const`, `static`, `constexpr`,
    /// `[[deprecated]]`, and whatever else a declaration may be preceded by.
    ///
    /// The innermost shape that **has** specifiers, which is the same direction [`DeclarationShapes::access_at`]
    /// reads and for the same reason: a name inside a function body is inside its declaration's shape, and the
    /// specifiers recorded there are that declaration's. A shape with none is not a declaration — a body, a class
    /// region — and is skipped rather than answering "no specifiers".
    ///
    /// The node rather than a reading of it: what a specifier *means* is a question each caller asks differently —
    /// a semantic highlighter wants `const`, a future layer might want `virtual` — and a function that returned one
    /// particular reading would be a second place for the list of keywords to live.
    pub fn specifiers_at(&self, offset: usize) -> Option<CppSyntaxNode> {
        let mut found = None;
        self.on_the_path(offset, |shape| {
            if let Some(specifiers) = &shape.specifiers {
                found = Some(specifiers.clone());
            }
        });
        found
    }

    /// **Is a declaration written at this offset exported** from the module the file declares?
    ///
    /// `false` outside every `export`ed region, which is the answer for an ordinary translation unit and for a
    /// module interface unit's private part alike — see [`Shape::exported`].
    pub fn exported_at(&self, offset: usize) -> bool {
        let mut found = false;
        self.on_the_path(offset, |shape| found = shape.exported);
        found
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// **The declaration node this name was written in** — the innermost shape containing it.
    ///
    /// The shape table's entries are the declaration nodes themselves ([`Shape::at`]), so this is the range a
    /// directive inside the declaration falls in — which is what "was this declaration split by the preprocessor"
    /// needs and what a fact's own range (its declarator) cannot answer.
    pub fn declaration_around(&self, name_range: SourceRange) -> Option<SourceRange> {
        let mut innermost = None;
        // **A `template<…>` header is where the directive usually sits**: `template<typename... _Args>` then
        // `#if __cplusplus > 201402L` … `#endif` then the declarator. The `TemplateDecl`'s own range covers both, so it
        // is preferred over the declaration inside it.
        let mut template = None;
        self.on_the_path(name_range.start_offset, |shape| {
            if !contains(shape.at, name_range) {
                return;
            }
            if shape.kind == cpp_parser::CppSyntaxKind::TemplateDecl {
                template = Some(shape.at);
            } else {
                innermost = Some(shape.at);
            }
        });
        template.or(innermost)
    }

    /// Walk the shapes an offset is inside, **outermost first**, and let the caller keep what it wants.
    ///
    /// This is the whole of the query side, and the reason it is a walk with a visitor rather than four functions
    /// that each return "the node I wanted": every question here is "the *innermost*/*outermost* node on the path
    /// with this property", and answering all of them from one chain is what stops the four questions from being
    /// four descents.
    ///
    /// The direction is the one a descent has — the original code walked down from the root and let each match
    /// **overwrite** the last, so "the innermost" is the *last* shape the visitor sees and "the outermost" is the
    /// first. Getting that backwards is not a subtle wrong answer: the outermost declaration of a class is a
    /// `DeclSpecifierSeq` whose text is the whole class body, so a member's type came out as `size; }`.
    fn on_the_path(&self, offset: usize, mut visit: impl FnMut(&Shape)) {
        let mut run = self.roots;

        loop {
            let candidates = &self.children[run.0 as usize..(run.0 + run.1) as usize];
            // Siblings are ordered and do not overlap, so the last one starting at or before the offset is the
            // only one that can contain it.
            let position = candidates
                .partition_point(|index| self.shapes[*index as usize].at.start_offset <= offset);
            let Some(&child) = position.checked_sub(1).and_then(|at| candidates.get(at)) else {
                return;
            };

            let shape = &self.shapes[child as usize];
            if shape.at.end_offset() <= offset {
                return;
            }

            visit(shape);
            run = shape.children;
        }
    }
}

/// Which slot of the run table a shape's children belong in: the roots' run first, then one run per shape.
fn slot_of(parent: Option<u32>) -> usize {
    parent.map_or(0, |parent| parent as usize + 1)
}

/// The children of `node` that a question in this module reads, in **one** scan of its children.
struct ShapeChildren {
    specifiers: Option<CppSyntaxNode>,
    trailing: Option<CppSyntaxNode>,
    declarator: Option<CppSyntaxNode>,
    type_id: Option<CppSyntaxNode>,
    bases: Vec<CppSyntaxNode>,
}

impl ShapeChildren {
    fn is_empty(&self) -> bool {
        self.specifiers.is_none()
            && self.trailing.is_none()
            && self.declarator.is_none()
            && self.type_id.is_none()
            && self.bases.is_empty()
    }
}

fn children_a_question_reads(node: &CppSyntaxNode) -> ShapeChildren {
    use cpp_parser::{
        CppAstNode, CppBaseSpecifier, CppDeclSpecifierSeq, CppDeclarator, CppTrailingReturnType,
        CppTypeId,
    };
    // **Five questions, five typed children.** The first of each kind is the one the shape keeps, and a base's *name* rather
    // than the specifier's text is what a fact's `bases` carries (`public B` is `B`).
    ShapeChildren {
        specifiers: cpp_parser::typed_children::<CppDeclSpecifierSeq>(node)
            .next()
            .map(|child| child.syntax().clone()),
        trailing: cpp_parser::typed_children::<CppTrailingReturnType>(node)
            .next()
            .map(|child| child.syntax().clone()),
        declarator: cpp_parser::typed_children::<CppDeclarator>(node)
            .next()
            .map(|child| child.syntax().clone()),
        type_id: cpp_parser::typed_children::<CppTypeId>(node)
            .next()
            .map(|child| child.syntax().clone()),
        bases: cpp_parser::typed_children::<CppBaseSpecifier>(node)
            .filter_map(|base| base.get_name())
            .map(|name| name.syntax().clone())
            .collect(),
    }
}

fn declares_a_shape(kind: CppSyntaxKind) -> bool {
    matches!(
        kind,
        CppSyntaxKind::UsingDecl
            | CppSyntaxKind::TypedefDecl
            | CppSyntaxKind::ClassDef
            | CppSyntaxKind::StructDef
            // **`UnionDef` is a shape too, and it was the one kind missing.** `enclosing_class_of` recognises it —
            // it is a class-like declaration whose members have access levels and a scope — so a shape table without
            // it is a table that cannot answer "what class is this member in" for a union, and the union's members
            // were the only ones still paying a descent from the root.
            | CppSyntaxKind::UnionDef
            | CppSyntaxKind::EnumDef
            // **A namespace and a concept declare no specifiers, and are shapes for a different
            // reason: their own names have to be *on the path*.** The same held for `export
            // namespace math { template <typename T> concept Number = …; }`: a concept has no
            // declarator either, so `math::Number` was invisible to importers.
            | CppSyntaxKind::NamespaceDecl
            | CppSyntaxKind::ConceptDecl
    )
}

/// **The namespace a namespace alias names** — `namespace views = ranges::views;` records `ranges::views`.
///
/// A namespace alias declares no members of its own, so a query that asks "what is written in `std::views`" finds
/// nothing while every name a reader wants is in the target. Measured on the corpus's `ranges.cpp`, which includes
/// `<ranges>`: completion after `std::views::` answered **0** names, while `filter`, `transform`, `reverse`, `drop`
/// and `take` are all declared in `std::ranges::views`.
///
/// The **target is recorded as written**, not resolved: which scope it names depends on the scope the alias sits in
/// (`ranges::views` inside `std` is `std::ranges::views`), and the query that reads this has the alias's own scope in
/// the same fact — see `names_in_a_scope` in the index.
fn declared_namespace_alias_target_with(root: &CppSyntaxNode, binding: &Binding) -> Option<String> {
    if binding.kind != BindingKind::Namespace {
        return None;
    }
    // **The innermost `namespace` declaration the name is written in.** Found by walking the name **token**'s
    // ancestors rather than by scanning the tree: the innermost enclosing declaration is at most a few parents up, and
    // the scan this replaced was measured at **16.7 s of a 17 s summary** on a 1.5 MB header — which is why a guard on
    // the text after the name existed, and the guard compared a rendering's bytes with a binding's range.
    use cpp_parser::CppNamespaceDecl;
    let token = cpp_parser::token_at(root, binding.name_range.start_offset)?;
    let namespace = cpp_parser::typed_ancestors::<CppNamespaceDecl>(&token.parent()?).next()?;
    // A declaration with a body is an ordinary namespace; an alias writes `= target` and no braces, and a forward
    // declaration `namespace std;` has neither — so the absence of a body is what tells the two apart.
    if namespace.get_body().is_some() {
        return None;
    }
    Some(namespace.get_name()?.get_qualified_name())
}

/// The type a **`typedef` or `using` alias** names, as the file spells it.
///
/// The third of the family [`declared_type_of`], [`declared_returns_of`], [`declared_bases_of`] — and the one
/// whose answer goes into the *same* field as the first: a fact's `type_of` is "the type this declaration is
/// about", which for a variable is the type it has and for an alias is the type it **points at**. An alias
/// declares no members of its own, so a consumer that reads a name written as `std::string` has to be able to get
/// from it to `std::basic_string`, and this is where that spelling comes from. See [`DeclFact::type_of`] for the
/// rule that tells the two apart.
///
/// # The two spellings a C++ alias comes in
///
/// ```text
/// using String = basic_string<char>;      the target is the TypeId after the `=`
/// typedef basic_string<char> String;      the target is the specifiers *and* the declarator
/// ```
///
/// The second is the reason this is not simply "the specifier sequence", which is what [`declared_type_of`] reads:
/// the alias's name is inside the declarator, and for `typedef void (*F)(int);` the specifiers alone are `void`.
/// So the target is the specifier text followed by the declarator **with the name cut out** — `(*)(int)` — which
/// joins into `void (*)(int)`: the spelling of the function-pointer type, arrived at by deleting the one word that
/// is the alias rather than the type. For the plain shape the cut leaves nothing and the answer is the specifiers,
/// as it should be.
///
/// # Why it is public
///
/// The query layer needs the same answer for an alias declared in the buffer it is looking at, without a summary:
/// the same reason [`declared_type_of`] is public, and the same rule about one implementation.
pub fn declared_alias_target(root: &CppSyntaxNode, binding: &Binding) -> Option<String> {
    declared_alias_target_with(&DeclarationShapes::of(root), binding)
}

/// [`declared_alias_target`] for a caller that has the file's shapes already.
fn declared_alias_target_with(shapes: &DeclarationShapes, binding: &Binding) -> Option<String> {
    if !matches!(binding.kind, BindingKind::Alias | BindingKind::Typedef) {
        return None;
    }

    // The innermost `using`/`typedef` the name is inside — "the last one passed on the way down". What is kept is
    // copied out rather than borrowed: the visitor is handed a reference valid only for its own call, which is the
    // shape of the walk (see [`DeclarationShapes::on_the_path`]).
    let mut declaration = None;
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        if matches!(
            shape.kind,
            CppSyntaxKind::UsingDecl | CppSyntaxKind::TypedefDecl
        ) {
            declaration = Some((
                shape.kind,
                shape.type_id.clone(),
                shape.specifiers.clone(),
                shape.declarator.clone(),
            ));
        }
    });
    let (kind, type_id, specifiers, declarator) = declaration?;

    if kind == CppSyntaxKind::UsingDecl {
        // `using X = <TypeId>;` — the target is that node's whole text, template arguments and all.
        return type_id
            .as_ref()
            .map(|target| target.text().to_string().trim().to_string())
            .filter(|target| !target.is_empty());
    }

    let specifiers = specifiers.as_ref()?;
    let declarator = declarator.as_ref();

    // **A `typedef` of a class body names the class, not the body's text.**
    //
    // ```c
    // typedef struct { int a; int b; } Object;      // the only name this type has is `Object`
    // typedef struct Tag { int d; } Alias;           // the members are filed under `Tag`
    // ```
    //
    // The general rule below builds the target as "the specifiers minus the declarator's name", which for a body
    // is the *definition's text*: `struct { int a; int b; }`. Nothing declares that spelling, so following the
    // alias ended in a name no scope has — measured as the whole of `members_of("Object")` answering an **empty**
    // list with `unlisted ["{inta;intb;}"]` while its two members sat under the scope `Object`, and as the
    // **21 539 of 40 000** refusals in neovim's member questions. C declares almost every struct this way, so the
    // shape is the common one rather than a corner.
    if let Some(named) = the_class_a_typedef_body_names(specifiers, &binding.name.text()) {
        return Some(named);
    }

    // No declarator: `typedef struct { … } ;` has no name to bind either, so this cannot be reached with a fact
    // to build — but returning the specifiers is the honest answer if it ever is.
    let Some(declarator) = declarator else {
        let spelling = strip_declaration_specifiers(&specifiers.text().to_string());
        return (!spelling.is_empty()).then_some(spelling);
    };

    let spelling = format!(
        "{} {}",
        strip_declaration_specifiers(&specifiers.text().to_string()),
        without(
            &declarator.text().to_string(),
            declarator,
            binding.name_range
        )
    );
    let spelling = spelling.split_whitespace().collect::<Vec<_>>().join(" ");

    (!spelling.is_empty()).then_some(spelling)
}

/// **The name a `typedef`'s class body gives the type it declares** — the tag when the body has one, and the
/// typedef's own name when it does not.
///
/// See the call site for the measurement. `typedef_name` is the fallback rather than a guess: a `typedef` is
/// exactly the construct that gives an unnamed class a name, which is why `typedef struct { … } Object;` can be
/// used as a type at all — so `Object` *is* the type's name, and any other spelling is one nothing declares.
fn the_class_a_typedef_body_names(
    specifiers: &CppSyntaxNode,
    typedef_name: &str,
) -> Option<String> {
    // **The class or enum this declaration defines**, by the kinds its typed node covers: `CppClassDef` is
    // `class`/`struct`/`union`, and a scoped enum is `EnumClassDef` (which is why `CppEnumDef::can_cast` cannot be the test
    // here — it also accepts a `BuiltinType`, and a specifier sequence has those for ordinary types).
    use cpp_parser::CppAstNode;
    let body = specifiers.children().find(|child| {
        let kind = CppSyntaxKind::from(child.kind());
        cpp_parser::CppClassDef::can_cast(kind)
            || matches!(kind, CppSyntaxKind::EnumDef | CppSyntaxKind::EnumClassDef)
    })?;

    let tag = cpp_parser::typed_children::<cpp_parser::CppNameExpr>(&body)
        .next()
        .map(|name| cpp_parser::subtree_text_spaced(name.syntax()))
        .filter(|written| !written.is_empty());

    Some(tag.unwrap_or_else(|| typedef_name.trim().to_string()))
}

/// `text` with the span `name` occupies removed.
///
/// The one place a spelling has to be *edited* rather than read, because a `typedef`'s type is its specifiers
/// plus its declarator and the alias's own name sits inside the second: cutting it out is what turns `(*F)(int)`
/// into `(*)(int)`. Offsets are byte offsets into the same source, so this is arithmetic on the two ranges rather
/// than a search for a word — a search would cut the wrong one in `typedef int int32_t;`(the second `int` is the
/// name, and the first is the type).
fn without(text: &str, node: &CppSyntaxNode, name: cpp_parser::SourceRange) -> String {
    let start = usize::from(node.text_range().start());
    let (from, to) = (
        name.start_offset.saturating_sub(start),
        name.end_offset().saturating_sub(start),
    );

    if from > to || to > text.len() || !text.is_char_boundary(from) || !text.is_char_boundary(to) {
        return text.to_string();
    }

    format!("{}{}", &text[..from], &text[to..])
}

/// The type a **function** declaration returns, as the file spells it.
///
/// The sibling of [`declared_type_of`], with the same walk down to the declaration and the same two answers — a
/// spelling, or `None` — and two differences that are the whole of it:
///
/// * only a **function** has one: `int x;` returns nothing, and `struct S { };` *is* a type rather than returning
///   one;
/// * a **trailing return type wins**: `auto make() -> Widget` spells the type *after* the parameter list, so the
///   specifier sequence says `auto` and the answer is in a `TrailingReturnType` the walk passes on the way down.
///   That node exists so a consumer does not have to strip the `->` itself, which is why this reads it rather
///   than the text.
///
/// `None` for a return type the file does not *state*: a deduced `auto` is not a class to look a member up in, and
/// recording it as one would answer `make().size` with "the type `auto` has no members" — a wrong answer where
/// "nothing is known" is the true one. See [`DeclFact::returns`].
pub fn declared_returns_of(root: &CppSyntaxNode, binding: &Binding) -> Option<String> {
    let written = declared_returns_of_with(&DeclarationShapes::of(root), binding);
    if written.is_some() {
        return written;
    }
    deduced_returns_of(root, binding)
}

/// **The type a function with no written return type returns** — `auto f() { … }` and `decltype(auto) f()`.
///
/// This is the half [`declared_returns_of_with`] refuses by design: `auto` is not a class anything can be looked
/// up in, so recording it would answer "no member `size` in `auto`" where the honest answer is that the file never
/// said. What the file *did* say is in the body, and this reads it.
///
/// # What is provable here, and what is not
///
/// A function's return type is the type of its `return` statements, and this layer is **syntax** — it has the file's
/// tree and its bindings, and no index, no scopes and no argument types. So it types a `return` only where the
/// syntax alone settles it, which today is:
///
/// * a **cast**: `return static_cast<T>(x);` has the type `T`, whatever `x` is, which is C++'s rule rather than an
///   inference — and it is the shape the standard library's helpers use to state a type they cannot spell.
///
/// Everything else is refused: `return f(x);` needs the callee's declaration, `return x;` needs the parameter's
/// type, and `return a + b;` needs both operands — each of them a question for a layer that has the index. A
/// **guess** here would be worse than the refusal, because `returns` is read by the member lookup: a wrong type
/// offers the wrong members, while a missing one offers none.
///
/// All the `return` statements must agree, which is C++'s own requirement. `if constexpr` makes that requirement
/// subtler than it looks — only one branch is instantiated per instantiation — so a body whose returns disagree is
/// refused rather than resolved by picking one.
/// [`deduced_returns_of`] for a caller that knows what the template parameters stand for.
///
/// # Why this exists separately, and why it is the call site that asks
///
/// A fact is built with **no call site in hand** — the index reads a file and records what it declares — so a body
/// that branches on a template parameter has no single answer when the fact is made. At a call site both halves
/// exist: the argument types say what the parameters stand for, and the declaring file can be read again. This is the
/// second reading, and it is what lets `if constexpr` **pick its branch**: with `_Iter = int*`, the condition
/// `is_pointer_v<decay_t<_Iter>>` holds, the `else` branches are not instantiated, and the returns that remain agree
/// on `_Iter&&` — where before they were `_Iter&&`, an unknown member call and `_Iter&&`, and the one unknown took
/// the whole answer away.
///
/// # What it does not do
///
/// Only the traits decidable from a **spelling** are evaluated — `is_pointer_v`, `is_integral_v`, `is_same_v` and
/// `decay_t` over them. A condition it cannot decide leaves every branch in play, which is what this layer did before
/// and is the honest answer: not knowing which branch a compiler takes is not a licence to pick one.
pub fn deduced_returns_of_substituted(
    root: &CppSyntaxNode,
    binding: Binding,
    substitutions: &crate::sema::types::TypeSubstitutions<'_>,
    // **The types the call passed**, in order — what a *pack* stands for. A fold expression widens all of them
    // (`sum_of(1, 2.5)` is a `double`), and a pack has no single substitution to read that from.
    argument_types: &[crate::sema::types::Type],
) -> Option<String> {
    if binding.kind != BindingKind::Function {
        return None;
    }

    let declaration = declaration_of(root, &binding)?;
    let body = body_of(&declaration)?;
    let returns = returned_expressions_in(&body)
        .into_iter()
        .filter(|expression| a_branch_that_is_taken(expression, &body, substitutions))
        .collect::<Vec<_>>();
    if returns.is_empty() {
        return None;
    }

    let mut agreed: Option<String> = None;
    for expression in &returns {
        let written =
            type_a_return_states_with(expression, &declaration, substitutions, argument_types)?;
        match &agreed {
            None => agreed = Some(written),
            Some(previous) if *previous == written => {}
            Some(_) => return None,
        }
    }
    agreed
}

/// **Is the `if constexpr` this return sits in one the compiler would instantiate?**
///
/// `true` for a return that is not inside a conditional at all, and for one whose enclosing conditions all hold under
/// the substitutions. A condition that cannot be decided answers `true` as well — leaving the return in play is what
/// this layer did before it could read conditions, and dropping a return because a trait was unfamiliar would turn an
/// unknown into a wrong answer.
fn a_branch_that_is_taken(
    expression: &cpp_parser::CppExpr,
    body: &cpp_parser::CppCompoundStat,
    substitutions: &crate::sema::types::TypeSubstitutions<'_>,
) -> bool {
    use cpp_parser::{CppAstNode, CppIfStat};
    // **The `if constexpr` statements that enclose this return**, innermost first — an ancestor walk. Asking the body for all of
    // its descendants and keeping the ones whose range contains the expression was the same answer at O(body) per return, plus a
    // `Vec` and a sort; the chain is O(depth), and an `if` that merely *follows* the return is not on it at all.
    //
    // The walk stops at the body that owns the expression: an `if` above it belongs to another function (a lambda's body inside
    // an `if`, for instance), and the old containment filter is what kept it out.
    let body_range = body.get_range();
    for condition in cpp_parser::typed_ancestors::<CppIfStat>(expression.syntax()) {
        if !body_range.contains_range(condition.get_range()) {
            break;
        }
        let Some(written) = constexpr_condition_of(&condition) else {
            continue;
        };
        let Some(held) = evaluate_a_trait(&written, substitutions) else {
            // A condition this layer cannot decide leaves the return in play, which is what it did before it could read
            // conditions at all.
            continue;
        };

        // **Which side of the `if` the return is on**, because the condition alone does not say whether the return is
        // instantiated: both branches are inside the same statement. A return after the `else` is in the branch taken when the
        // condition is **false**; one before it is in the branch taken when it is **true**.
        let in_the_else = condition.get_else_branch().is_some_and(|else_branch| {
            expression.get_range().start() > else_branch.get_range().start()
        });
        if held == in_the_else {
            return false;
        }
    }
    true
}

/// The parenthesised condition of an `if constexpr`, when that is what this statement is.
///
/// Asked of the tree: `CppIfStat::is_constexpr` is the keyword question and `get_condition` the expression — the text search
/// this used to do (`find("constexpr")`, then balance the parentheses by hand) answered nothing on a condition written with
/// the `constexpr` on its own line, and read a `constexpr` inside a nested lambda as this `if`'s.
fn constexpr_condition_of(statement: &cpp_parser::CppIfStat) -> Option<String> {
    use cpp_parser::CppAstNode;
    if !statement.is_constexpr() {
        return None;
    }
    let inner = statement.get_condition()?.get_expr()?;
    Some(cpp_parser::subtree_text_spaced(inner.syntax()))
}

/// **What a type trait says about a spelling** — `None` for a condition this layer cannot decide.
///
/// Spelling-level, like every comparison in this file, and deliberately narrow: only the traits whose answer follows
/// from the **written form** of the type are decided, because those need no instantiation.
///
/// ```text
/// decay_t<X>       a reference and top-level `const` come off. What `decay` does to arrays and functions is not
///                  attempted — no spelling in this layer distinguishes them yet.
/// is_pointer_v<X>  X ends with `*`. A smart pointer is not one, and this says so.
/// is_integral_v<X> X is one of the builtin integer names. A `typedef` of one is not recognised, which is the
///                  direction that costs an answer rather than giving a wrong one.
/// is_same_v<A, B>  the two spellings are equal after `decay_t` and whitespace.
/// ```
fn evaluate_a_trait(
    condition: &str,
    substitutions: &crate::sema::types::TypeSubstitutions<'_>,
) -> Option<bool> {
    let condition = condition.trim();

    if let Some(rest) = condition.strip_prefix('!') {
        return evaluate_a_trait(rest, substitutions).map(|held| !held);
    }
    if let Some((left, right)) = condition.split_once("&&") {
        return match (
            evaluate_a_trait(left, substitutions),
            evaluate_a_trait(right, substitutions),
        ) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        };
    }
    if let Some((left, right)) = condition.split_once("||") {
        return match (
            evaluate_a_trait(left, substitutions),
            evaluate_a_trait(right, substitutions),
        ) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        };
    }

    let (name, argument) = condition.split_once('<')?;
    let argument = argument.trim_end().strip_suffix('>')?;
    let (first, second) = match argument.split_once(',') {
        Some((first, second)) => (first.trim(), Some(second.trim())),
        None => (argument.trim(), None),
    };
    let first = substituted_spelling(first, substitutions);

    match name.trim() {
        "is_pointer_v" => Some(first.ends_with('*')),
        "is_integral_v" => Some(matches!(
            first.as_str(),
            "bool"
                | "char"
                | "signed char"
                | "unsigned char"
                | "short"
                | "unsigned short"
                | "int"
                | "unsigned int"
                | "long"
                | "unsigned long"
                | "long long"
                | "unsigned long long"
                | "wchar_t"
                | "char8_t"
                | "char16_t"
                | "char32_t"
                | "size_t"
                | "ptrdiff_t"
        )),
        "is_same_v" => {
            let second = substituted_spelling(second?, substitutions);
            Some(first == second)
        }
        _ => None,
    }
}

/// A spelling with `decay_t<…>` applied and the template parameters substituted — see [`evaluate_a_trait`].
fn substituted_spelling(
    written: &str,
    substitutions: &crate::sema::types::TypeSubstitutions<'_>,
) -> String {
    let mut text = written.trim();
    while let Some(inner) = text
        .strip_prefix("decay_t<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        text = inner.trim();
    }

    let replaced = substitutions.get(text).map(|argument| argument.to_string());
    let text = replaced.as_deref().unwrap_or(text);
    strip_reference_and_const(text)
}

/// A type spelling with its references and top-level `const` off — what `decay` does to the part this layer knows.
fn strip_reference_and_const(written: &str) -> String {
    let mut text = written.trim();
    loop {
        let trimmed = text.trim_end();
        let without = trimmed
            .strip_suffix("&&")
            .or_else(|| trimmed.strip_suffix('&'));
        match without {
            Some(without) => text = without.trim_end(),
            None => break,
        }
    }

    let mut words: Vec<&str> = text.split_whitespace().collect();
    while words
        .first()
        .is_some_and(|word| matches!(*word, "const" | "volatile"))
        && !words.iter().any(|word| word.contains('*'))
    {
        words.remove(0);
    }
    words.join(" ")
}

/// **The parameters this function frees**, as a bit per parameter index — see [`crate::DeclFact::frees`].
///
/// Only an **unconditional** free is recorded: a `delete` at the top level of the body, or inside a plain block (which
/// is the same statement sequence), frees its parameter on every path that reaches the end of the function. A `delete`
/// inside an `if`, a loop, a `switch` or a `try` is not recorded, because a caller that uses the pointer afterwards is
/// only *sometimes* wrong — and the rule that reads this reports only what is certain.
///
/// ```text
///   void release(T* p) { delete p; }                 recorded: bit 0
///   void maybe(T* p)   { if (c) delete p; }          not recorded: the free may not run
///   void drop(T* p)    { free(p); }                  recorded: bit 0
///   void end(T* p)     { std::destroy_at(p); }       recorded: bit 0
/// ```
///
/// # Why one level
///
/// A function that frees through another call (`void release(T* p) { hand_off(p); }`) is not recorded, because this
/// answer is written **while the project is being read** and the callee's fact may not exist yet. The one-level rule
/// covers the shape that matters — the helper whose whole job is to free — and a second pass over the index could
/// close the chain later.
pub(crate) fn freed_parameters_of(root: &CppSyntaxNode, binding: &Binding) -> u64 {
    // **Only a function can free one of its arguments**, and the guard is what keeps this from walking the tree for
    // every variable of a file: the caller that needs it is a member query, one call at a time.
    if !matches!(
        binding.kind,
        BindingKind::Function | BindingKind::Constructor
    ) {
        return 0;
    }
    // The innermost declaration that reaches the name and has a body: a free is written in a body or nowhere.
    use cpp_parser::CppAstNode;
    let name_start = binding.name_range.start_offset;
    // **The innermost declaration that reaches the name and has a body**: a free is written in a body or nowhere.
    let declaration = cpp_parser::typed_descendants::<cpp_parser::CppDeclaration>(root)
        .filter(|declaration| {
            let range = declaration.get_range();
            usize::from(range.start()) <= name_start
                && name_start < usize::from(range.end())
                && declaration.get_body().is_some()
        })
        .max_by_key(|declaration| usize::from(declaration.get_range().start()));
    let Some(declaration) = declaration else {
        return 0;
    };
    let Some(body) = declaration.get_body() else {
        return 0;
    };
    let parameters = the_parameter_names(declaration.syntax());
    if parameters.is_empty() {
        return 0;
    }

    let mut mask = 0u64;
    scan_unconditional_statements(body.syntax(), &parameters, &mut mask);
    mask
}

/// **Fill [`DeclFact::frees`](crate::DeclFact::frees) for every function of one file**, in a single walk.
///
/// The map is built once — every declaration that has a body, keyed by the offset its name starts at — and then each
/// fact is answered from it. The alternative, asked per fact, is a walk of the whole rendering per declaration, which
/// on a file that includes two standard headers is minutes rather than milliseconds.
fn fill_the_frees(root: &CppSyntaxNode, facts: &mut [crate::DeclFact]) {
    if facts.is_empty() {
        return;
    }
    let mut bodies: std::collections::HashMap<usize, (CppSyntaxNode, Vec<String>)> =
        std::collections::HashMap::new();
    use cpp_parser::CppAstNode;
    // **The name and the body of every declaration that has one**, asked of the typed node: `get_name` covers the class, the
    // namespace, the declarator and the constructor's bare declarator, which is the chain four kind comparisons used to spell
    // out.
    for node in cpp_parser::typed_descendants::<cpp_parser::CppDeclaration>(root) {
        let Some(body) = node.get_body() else {
            continue;
        };
        let Some(name) = node.get_name() else {
            continue;
        };
        let name_start = usize::from(cpp_parser::CppAstToken::get_range(&name).start());
        let parameters = the_parameter_names(node.syntax());
        bodies.insert(name_start, (body.syntax().clone(), parameters));
    }

    for fact in facts {
        if fact.kind != crate::DeclKind::Function {
            continue;
        }
        let Some((body, parameters)) = bodies.get(&fact.name_range.start_offset) else {
            continue;
        };
        if parameters.is_empty() {
            continue;
        }
        let mut mask = 0u64;
        scan_unconditional_statements(body, parameters, &mut mask);
        fact.frees = mask;
    }
}

/// **The reflection annotation each fact was written under**, read from the file's text.
///
/// Unreal's reflection macros expand to nothing, so the tree a fact is built from holds no `UCLASS` and no specifier —
/// the text is the only place they still exist:
///
/// ```text
/// UPROPERTY(EditAnywhere, Category = "Game")
/// bool bIsReady = false;                     <- the declaration starts at `bool`
/// ```
///
/// **The declaration's start, not the fact's**: a variable's fact begins at its *declarator* (`bIsReady`), and the text
/// before that is `bool ` rather than the annotation — measured, and the first version of this pass matched nothing on
/// members because of it. So the tree is walked once, the annotation is read before each declaration's own start, and
/// the result is filed under the **declared name's** offset, which is what a fact carries in `name_range`.
fn fill_the_reflection(
    root: &cpp_parser::CppSyntaxNode,
    written: &str,
    facts: &mut [crate::DeclFact],
) {
    let mut annotations: std::collections::HashMap<usize, crate::summary::ReflectionFact> =
        std::collections::HashMap::new();
    use cpp_parser::CppAstNode;
    for node in cpp_parser::typed_descendants::<cpp_parser::CppDeclaration>(root) {
        let Some(name_start) = the_declaration_name_start(node.syntax()) else {
            continue;
        };
        // **Two shapes, because the parser reads them differently.** A class's `UCLASS(...)` is a sibling *before* the
        // declaration; a member function's `UFUNCTION(...)` was measured to be **part of** the declaration — the parser's
        // recovery keeps the macro call and the signature together — so the annotation starts at the declaration itself.
        let start = usize::from(node.get_range().start());
        let Some((macro_name, specifiers)) =
            the_annotation_before(written, start).or_else(|| the_annotation_at(written, start))
        else {
            continue;
        };
        annotations.insert(
            name_start,
            crate::summary::ReflectionFact {
                macro_name,
                specifiers,
            },
        );
    }
    for fact in facts {
        if fact.reflection.is_some() {
            continue;
        }
        if let Some(reflection) = annotations.get(&fact.name_range.start_offset) {
            fact.reflection = Some(reflection.clone());
        }
    }
}

/// **Where the name a declaration declares starts** — the class's own name, or the declarator's, never the first name in its
/// type.
fn the_declaration_name_start(declaration: &cpp_parser::CppSyntaxNode) -> Option<usize> {
    use cpp_parser::{CppAstNode, CppAstToken, CppDeclaration};
    let declaration = CppDeclaration::cast(declaration.clone())?;
    let name = declaration.get_name()?;
    Some(usize::from(name.get_range().start()))
}

/// The macro call that ends immediately before `at`, with its arguments split into specifiers.
fn the_annotation_before(written: &str, at: usize) -> Option<(String, Vec<String>)> {
    let before = written.get(..at)?.trim_end();
    if !before.ends_with(')') {
        return None;
    }
    let bytes = before.as_bytes();
    let mut depth = 0usize;
    let mut index = bytes.len();
    while index > 0 {
        index -= 1;
        match bytes[index] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    let head = before[..index].trim_end();
    // **A char offset, and a char's own width.** `rfind` answers the *byte* offset of a character, so `offset + 1` is
    // that character's successor only when it is one byte wide: `×` (U+00D7) is two, and `&head[start..]` panicked with
    // "start byte index 37747 is not a char boundary; it is inside '×'" on a real project — inside the **indexing**
    // thread, which is a crash rather than a wrong answer. `len_utf8` is the offset of the next character, which is
    // what this wants, and it is the same expression for every width.
    let start = head
        .char_indices()
        .rev()
        .find(|(_, ch)| !(ch.is_alphanumeric() || *ch == '_'))
        .map(|(offset, ch)| offset + ch.len_utf8())
        .unwrap_or(0);
    let macro_name = &head[start..];
    if !matches!(
        macro_name,
        "UCLASS" | "USTRUCT" | "UENUM" | "UINTERFACE" | "UPROPERTY" | "UFUNCTION"
    ) {
        return None;
    }
    Some((
        macro_name.to_string(),
        the_specifiers(&before[index + 1..before.len() - 1]),
    ))
}

/// The macro call that **starts** at `at`, with its arguments split into specifiers — the shape the parser leaves for a
/// member function, where the annotation and the signature are one declaration.
fn the_annotation_at(written: &str, at: usize) -> Option<(String, Vec<String>)> {
    let rest = written.get(at..)?;
    let open = rest.find('(')?;
    let macro_name = rest[..open].trim_end();
    if !matches!(
        macro_name,
        "UCLASS" | "USTRUCT" | "UENUM" | "UINTERFACE" | "UPROPERTY" | "UFUNCTION"
    ) {
        return None;
    }
    let mut depth = 0usize;
    let mut close = None;
    for (index, character) in rest[open..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + index);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    Some((
        macro_name.to_string(),
        the_specifiers(&rest[open + 1..close]),
    ))
}
/// **The specifiers of one annotation**, split at the top-level commas.
///
/// A value may carry its own parentheses and commas — `meta=(DisplayName="Ready", ToolTip="…")`, `Category="A, B"` — so
/// the split follows the nesting and the string literals rather than the text's commas.
fn the_specifiers(arguments: &str) -> Vec<String> {
    let mut specifiers = Vec::new();
    let mut depth = 0usize;
    let mut in_a_string = false;
    let mut start = 0usize;
    for (index, character) in arguments.char_indices() {
        match character {
            '"' => in_a_string = !in_a_string,
            '(' | '[' | '{' if !in_a_string => depth += 1,
            ')' | ']' | '}' if !in_a_string => depth = depth.saturating_sub(1),
            ',' if depth == 0 && !in_a_string => {
                let specifier = arguments[start..index].trim();
                if !specifier.is_empty() {
                    specifiers.push(specifier.to_string());
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    let specifier = arguments[start..].trim();
    if !specifier.is_empty() {
        specifiers.push(specifier.to_string());
    }
    specifiers
}

/// The parameter names of a function declaration, in the order they are written.
/// The parameters a **scope** declares: a function declaration's parameter list, or a lambda's.
///
/// One helper rather than a cast in each caller, because the two scopes are different node types and the question is the same.
/// A scope that is neither (a class body, say) has no parameters of its own and answers an empty list.
fn the_parameters_of(scope: &CppSyntaxNode) -> Vec<cpp_parser::CppParam> {
    use cpp_parser::{CppAstNode, CppDeclaration, CppLambdaExpr};
    // **The scope's own parameter list**, not a walk of its subtree: a function's body belongs to its declaration, so
    // `descendants()` reached every lambda in it and answered their parameters too (measured: `auto f(int a) { auto g =
    // [](int b) { … }; }` gave `f` two parameters). The list is one typed question away.
    if let Some(declaration) = CppDeclaration::cast(scope.clone()) {
        return declaration
            .get_param_list()
            .map(|list| list.get_params().collect())
            .unwrap_or_default();
    }
    if let Some(lambda) = CppLambdaExpr::cast(scope.clone()) {
        return lambda
            .get_params()
            .map(|list| list.get_params().collect())
            .unwrap_or_default();
    }
    Vec::new()
}

/// The declarations written **inside** a scope, whatever kind of scope it is.
fn the_declarations_in(scope: &CppSyntaxNode) -> Vec<cpp_parser::CppDeclaration> {
    use cpp_parser::{CppAstNode, CppDeclaration, CppLambdaExpr};
    // **The scope's body**, not its whole declaration: a local class's members and a nested lambda's locals are different
    // scopes, and a `descendants()` walk over the declaration reached both. The lambda branch is the inlay's use, where the
    // lambda itself is the scope.
    let body = if let Some(lambda) = CppLambdaExpr::cast(scope.clone()) {
        lambda.get_body()
    } else {
        CppDeclaration::cast(scope.clone()).and_then(|declaration| declaration.get_body())
    };
    let Some(body) = body else {
        return Vec::new();
    };
    body.descendants::<CppDeclaration>()
        .filter(|candidate| {
            // **Not one written inside a nested lambda**: that lambda's body is another function's scope.
            cpp_parser::typed_ancestors_until::<CppLambdaExpr>(candidate.syntax(), body.syntax())
                .next()
                .is_none()
        })
        .collect()
}

fn the_parameter_names(declaration: &CppSyntaxNode) -> Vec<String> {
    the_parameters_of(declaration)
        .into_iter()
        .filter_map(|parameter| parameter.get_name_text())
        .collect()
}

/// Walk the statements that **always run**, and record the frees among them.
fn scan_unconditional_statements(node: &CppSyntaxNode, parameters: &[String], mask: &mut u64) {
    use cpp_parser::{CppAstNode, CppStat};
    for child in node.children() {
        // **The sum type, not a kind comparison**: a block is the same sequence, an expression statement and a `return` may
        // free a parameter, and everything else may not run.
        match CppStat::cast(child.clone()) {
            Some(CppStat::CompoundStat(block)) => {
                scan_unconditional_statements(block.syntax(), parameters, mask);
            }
            Some(CppStat::ExpressionStat(_) | CppStat::ReturnStat(_) | CppStat::Declaration(_)) => {
                scan_for_a_free(&child, parameters, mask);
            }
            _ => {}
        }
    }
}

/// Record the frees written in one statement's expressions, without descending into a lambda's body.
fn scan_for_a_free(node: &CppSyntaxNode, parameters: &[String], mask: &mut u64) {
    use cpp_parser::{CppAstNode, CppExpr};
    if let Some(expression) = CppExpr::cast(node.clone()) {
        match &expression {
            // A lambda's body is another function: its frees are not this one's.
            CppExpr::LambdaExpr(_) => return,
            CppExpr::UnaryExpr(unary) => {
                // `delete p;` is a unary expression whose operator is the `delete` keyword — `delete[] p` the same.
                if unary.is_delete()
                    && let Some(operand) = unary.get_operand()
                    && let Some(name) = a_bare_name(operand.syntax())
                    && let Some(index) = parameters.iter().position(|parameter| parameter == &name)
                    && index < 64
                {
                    *mask |= 1 << index;
                }
            }
            CppExpr::CallExpr(call) => {
                let name = call
                    .get_callee()
                    .and_then(|callee| the_last_identifier_of(callee.syntax()));
                let frees_its_first_argument = match name.as_deref() {
                    Some("free") => true,
                    // `std::destroy_at(p)` ends the object's lifetime, which a later use reads.
                    Some("destroy_at") => true,
                    _ => false,
                };
                if frees_its_first_argument
                    && let Some(argument) = call.get_args().into_iter().next()
                    && let Some(name) = a_bare_name(argument.syntax())
                    && let Some(index) = parameters.iter().position(|parameter| parameter == &name)
                    && index < 64
                {
                    *mask |= 1 << index;
                }
            }
            _ => {}
        }
    }
    for child in node.children() {
        scan_for_a_free(&child, parameters, mask);
    }
}
/// The name a frees-its-argument expression names: `p` for `p` and for `&p`.
fn a_bare_name(expression: &CppSyntaxNode) -> Option<String> {
    use cpp_parser::{CppAstNode, CppNameExpr, CppUnaryExpr};
    if let Some(name) = CppNameExpr::cast(expression.clone()) {
        return name.get_name_text();
    }
    // `&p` and `*p` name `p`: the operand is the expression that names it.
    let unary = CppUnaryExpr::cast(expression.clone())?;
    a_bare_name(unary.get_operand()?.syntax())
}

/// The last identifier in an expression, as a spelling.
fn the_last_identifier_of(expression: &CppSyntaxNode) -> Option<String> {
    use cpp_parser::{CppAstNode, CppCallExpr, CppMemberExpr, CppNameExpr};
    // A qualified name's last segment is the one the token layer can name directly — `std::free` is `free`.
    if let Some(name) = CppNameExpr::cast(expression.clone()) {
        return name
            .get_last_name_token()
            .map(|token| token.get_name_text().to_string());
    }
    // `p->free(…)` and `object.release(…)`: the member is the name the call goes through.
    if let Some(member) = CppMemberExpr::cast(expression.clone()) {
        return member
            .get_member_name()
            .map(|token| token.get_name_text().to_string());
    }
    // `std::free(p)`: the callee is the name.
    let call = CppCallExpr::cast(expression.clone())?;
    let callee = call.get_callee()?;
    the_last_identifier_of(callee.syntax())
}

fn deduced_returns_of(root: &CppSyntaxNode, binding: &Binding) -> Option<String> {
    if binding.kind != BindingKind::Function {
        return None;
    }

    let declaration = declaration_of(root, binding)?;
    let body = body_of(&declaration)?;
    let returns = returned_expressions_in(&body);
    if returns.is_empty() {
        return None;
    }

    let mut agreed: Option<String> = None;
    for expression in &returns {
        let written = type_a_return_states(expression, &declaration)?;
        match &agreed {
            None => agreed = Some(written),
            // Two returns of different types: the answer depends on which branch is instantiated, and this layer
            // does not instantiate. Note for the next reader: relaxing this to "agree once references and `const`
            // come off" — which is what an `auto` initializer strips, and so would be sound for the caller — was
            // written and **measured at no change** (306 deduced before and after), because the returns that
            // disagree never reach this comparison: `_Get_unwrapped` returns `_It + 0`, and that expression has no
            // type to compare until `_Iter` is substituted for `_It`. It belongs with instantiation.
            Some(_) => return None,
        }
    }
    agreed
}

/// The `CompoundStatement` a function's declarator is followed by, when the file writes one.
///
/// Asked of the **declarator the name sits in** rather than of the declaration's whole range, because a declaration
/// can hold several declarators (`int a, f() { … }` is not C++ but `auto f() -> T; auto g() { … }` in one scope is
/// The `CompoundStat` this declaration's body is written with, when the file writes one.
///
/// Asked of the **declaration the name sits in** rather than of the file, because a range that merely *contains* a
/// body would find the body of the next function along. See [`declaration_of`] for how that declaration is found,
/// and for why it is a `Declaration` rather than a `FunctionDef`.
fn body_of(declaration: &CppSyntaxNode) -> Option<cpp_parser::CppCompoundStat> {
    use cpp_parser::{CppAstNode, CppDeclaration, CppLambdaExpr};
    // A function's body is its declaration's; a lambda's is the lambda's. The two node types are the only ones a `return`
    // can be written in, which is why this is a question with two answers rather than a search.
    if let Some(declaration) = CppDeclaration::cast(declaration.clone()) {
        return declaration.get_body();
    }
    CppLambdaExpr::cast(declaration.clone())?.get_body()
}

/// **The innermost declaration the name sits in** — a function definition, as this tree writes one.
///
/// A function *definition* is not a node of its own here. Measured on `auto mixed(int* _First) { … }`, the ancestors
/// of the name are `TranslationUnit → Declaration → InitDeclarator → Declarator → NameExpr`, so a search for a
/// `FunctionDef` finds nothing and every deduced return type is refused for a reason that has nothing to do with the
/// deduction.
///
/// # Why this descends from the name's token rather than walking the file
///
/// It used to be `root.descendants()`, filtered to declarations containing the name, and the last one kept — a walk
/// of the **whole file** per question. The question is asked once per function that writes no return type, so the
/// cost was `(auto-return functions) × (file size)`: measured on GCC 15's `<ranges>` (92 654 tokens, 69 267 nodes,
/// 2 017 declarations), **173 such walks ≈ 570 ms of that file's 672 ms summary**, and 650 ms of a cold index of
/// this project (8 329 → 7 689 ms with the walk removed; the heaviest file's step 654 → 145 ms).
///
/// A token and its ancestors are the only nodes that can hold the name, so this is O(depth). `parent_ancestors`
/// is innermost-first, which is the "last containing node in document order" the old walk was after.
fn declaration_of(root: &CppSyntaxNode, binding: &Binding) -> Option<CppSyntaxNode> {
    use cpp_parser::CppAstNode;
    let token = cpp_parser::token_at(root, binding.name_range.start_offset)?;
    // `CppDeclaration::can_cast` is the declaration node; `FunctionDef` stays as a fallback for the shapes the recovery parser
    // can build, although a function definition is measured to be a `Declaration` here.
    token.parent_ancestors().find(|node| {
        cpp_parser::CppDeclaration::can_cast(node.kind().into())
            || cpp_parser::CppSyntaxKind::from(node.kind())
                == cpp_parser::CppSyntaxKind::FunctionDef
    })
}

/// Every expression a `return` statement in this body returns, in document order.
///
/// Nested functions and lambdas are **skipped**: their `return`s belong to them, and counting one here would make a
/// function's type depend on a body it does not own. `return;` contributes nothing rather than an empty expression.
/// **What a lambda returns**, deduced from its own body — `auto f = [](int v) { return v * 2; }; f(3)`.
///
/// A lambda has no name, so the type of the *variable* it is stored in is the only way to reach the body; the caller
/// resolves that variable and hands the lambda expression here. The parameters are the lambda's own, and there are no
/// template parameters to substitute — a **generic** lambda (`[](auto v)`) answers `None`, because `auto` there is a template
/// parameter written with another word and nothing in the call says what it stands for.
pub fn a_lambdas_return_type(
    lambda: &CppSyntaxNode,
    argument_types: &[crate::sema::types::Type],
) -> Option<String> {
    use cpp_parser::{CppAstNode, CppLambdaExpr};
    // **The lambda's own body and parameters**, and nothing else: a lambda has no name, so the caller resolves the variable
    // it is stored in and hands the expression here.
    let lambda = CppLambdaExpr::cast(lambda.clone())?;
    let body = lambda.get_body()?;
    let parameters = the_parameters_of(lambda.syntax());
    let receives_a_placeholder = parameters.iter().any(cpp_parser::CppParam::is_auto);
    // **A generic parameter takes the call's argument.** `[](auto v) { return v; }` called as `f(3)` returns an `int`, and
    // the arguments are what say so — the parameters written `auto` are paired with them by position. When the call wrote no
    // arguments (the inlay asks about the lambda itself), there is nothing to pair and the answer is refused.
    let placeholder_types = the_auto_parameters_with_their_arguments(&parameters, argument_types);
    if receives_a_placeholder && placeholder_types.is_empty() {
        return None;
    }
    let substitutions = crate::sema::types::TypeSubstitutions::new(&[], argument_types);
    let mut agreed: Option<String> = None;
    for statement in the_returns_of(&body, lambda.syntax()) {
        let Some(expression) = statement.get_expr() else {
            continue;
        };
        let written = match type_with_the_auto_parameters(&expression, &placeholder_types) {
            Some(written) => written,
            None => type_a_return_states_with(
                &expression,
                lambda.syntax(),
                &substitutions,
                argument_types,
            )?,
        };
        match &agreed {
            None => agreed = Some(written),
            Some(previous) if *previous == written => {}
            Some(_) => return None,
        }
    }
    agreed
}

/// **The `auto` parameters of a lambda, paired with the call's arguments** — `[](auto v) { … }` called `f(3)` gives `v → int`.
///
/// The pairing is positional, which is what the language does: the i-th parameter takes the i-th argument. A parameter
/// written `auto const&`/`auto&&` still receives the argument's type here — the reference and the `const` are the parameter's
/// shape, and what a `return` of the name then means is the value's type.
/// **The `auto` parameters of a lambda, paired with the call's arguments** — `[](auto v) { … }` called `f(3)` gives
/// `v → int`.
///
/// The pairing is positional, which is what the language does: the i-th parameter takes the i-th argument. A parameter written
/// `auto const&`/`auto&&` still receives the argument's type here — the reference and the `const` are the parameter's shape, and
/// what a `return` of the name then means is the value's type.
fn the_auto_parameters_with_their_arguments(
    parameters: &[cpp_parser::CppParam],
    argument_types: &[crate::sema::types::Type],
) -> Vec<(String, crate::sema::types::Type)> {
    parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| parameter.is_auto())
        .filter_map(|(at, parameter)| {
            Some((parameter.get_name_text()?, argument_types.get(at)?.clone()))
        })
        .collect()
}

/// **The type a return statement states, with the `auto` parameters standing for their arguments.**
///
/// The arms are the shapes a return is written with: a name (a paired parameter, or one the ordinary walk can type), a
/// literal, an arithmetic pair, parentheses, and a construction. `None` when the shape is anything else, and the caller
/// falls back to the ordinary walk — which is what a lambda with no `auto` parameters uses.
/// **Is this binary expression a comparison?** — the answer is `bool` whatever its operands are.
///
/// The operator's **spelling** is what decides, and it is the token's own (`CppBinaryExpr::get_operator_text`), not a search of
/// the expression's text for a character class.
fn is_a_comparison(binary: &cpp_parser::CppBinaryExpr) -> bool {
    matches!(
        binary.get_operator_text().as_deref(),
        Some("==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||")
    )
}

/// **Is this expression a literal zero?** — `X + 0` is `X`, because the zero is a count rather than a term.
///
/// The literal's own text is what C++ compares here: the *type* of `0` is `int`, so a test of the type can never fire.
fn is_a_literal_zero(expression: &cpp_parser::CppExpr) -> bool {
    let cpp_parser::CppExpr::LiteralExpr(literal) = expression else {
        return false;
    };
    literal.get_literal_text().is_some_and(|text| {
        matches!(
            text.trim(),
            "0" | "0L" | "0U" | "0UL" | "0LL" | "0ULL" | "0u" | "0ul" | "0ll" | "0ull"
        )
    })
}

/// **The type a return statement states, with the `auto` parameters standing for their arguments.**
///
/// The arms are the shapes a return is written with: a name (a paired parameter), a literal, an arithmetic pair, parentheses
/// and a construction. `None` when the shape is anything else, and the caller falls back to the ordinary walk — which is what a
/// lambda with no `auto` parameters uses.
fn type_with_the_auto_parameters(
    expression: &cpp_parser::CppExpr,
    paired: &[(String, crate::sema::types::Type)],
) -> Option<String> {
    use cpp_parser::{CppAstNode, CppExpr};
    match expression {
        CppExpr::ParenExpr(paren) => type_with_the_auto_parameters(&paren.get_expr()?, paired),
        CppExpr::NameExpr(name) => {
            let written = name.get_name_text()?;
            paired
                .iter()
                .find(|(known, _)| *known == written)
                .map(|(_, type_of)| type_of.to_string())
        }
        CppExpr::LiteralExpr(literal) => {
            Some(literal_type(&literal.get_literal_text()?).to_string())
        }
        CppExpr::InitListExpr(list) => {
            let head = list.get_constructed_type()?;
            Some(cpp_parser::subtree_text_spaced(head.syntax()))
        }
        CppExpr::BinaryExpr(binary) => {
            if is_a_comparison(binary) {
                return Some("bool".to_string());
            }
            let left_type = type_with_the_auto_parameters(&binary.get_lhs()?, paired)?;
            let right_type = type_with_the_auto_parameters(&binary.get_rhs()?, paired)?;
            // **The usual arithmetic conversion, in the one case needing no table**: equal types are that type, and a
            // floating type absorbs an integer. Anything else is the ranking this layer does not keep.
            if left_type == right_type {
                return Some(left_type);
            }
            let floating = |written: &str| written.contains("float") || written.contains("double");
            match (floating(&left_type), floating(&right_type)) {
                (true, true) => Some(if left_type.len() >= right_type.len() {
                    left_type
                } else {
                    right_type
                }),
                (true, false) => Some(left_type),
                (false, true) => Some(right_type),
                (false, false) => None,
            }
        }
        _ => None,
    }
}

/// **The `return` statements a body owns**, in document order.
///
/// A lambda written in the body is another function: its `return`s belong to it. `typed_ancestors_until` answers exactly that
/// question — is there a lambda between this statement and the body that owns it — so the filter needs no hand-written walk.
fn the_returns_of(
    body: &cpp_parser::CppCompoundStat,
    owner: &CppSyntaxNode,
) -> Vec<cpp_parser::CppReturnStat> {
    use cpp_parser::{CppAstNode, CppDeclaration, CppLambdaExpr, CppReturnStat};
    body.descendants::<CppReturnStat>()
        .filter(|statement| {
            // **A lambda or a local class's method is another function**: its returns are its own, and counting one here made a
            // function's type depend on a body it does not own.
            cpp_parser::typed_ancestors_until::<CppLambdaExpr>(statement.syntax(), owner)
                .next()
                .is_none()
                && cpp_parser::typed_ancestors_until::<CppDeclaration>(statement.syntax(), owner)
                    .all(|declaration| declaration.get_body().is_none())
        })
        .collect()
}

/// Every expression a `return` in this body returns, in document order; `return;` contributes nothing.
fn returned_expressions_in(body: &cpp_parser::CppCompoundStat) -> Vec<cpp_parser::CppExpr> {
    use cpp_parser::CppAstNode;
    the_returns_of(body, body.syntax())
        .into_iter()
        .filter_map(|statement| statement.get_expr())
        .collect()
}

/// The type this expression **states**, when the syntax alone states one.
///
/// Four shapes, and each is a *reading* rather than an inference — the type is written in the expression or in a
/// declaration beside it:
///
/// * **a cast**: `return static_cast<T>(x);` has the type `T`, whatever `x` is.
/// * **a literal**: `return 1;` is an `int`, and no declaration anywhere says otherwise.
/// * **a parameter**: `return _First;` has the parameter's own declared type, which is in this function's
///   parameter list — the same tree, one walk away.
/// * **a binary whose operands agree**: `return a + b;` with both `int` is an `int`, which is C++'s usual
///   arithmetic conversion in the one case that needs no table (see `type_of_expression`'s note on ranking).
///
/// Everything else is refused. `return f(x);` needs the callee's declaration, which is in another file and another
/// layer; a member access needs the object's type; and two operands that *disagree* need the conversion ranks this
/// deliberately does not keep.
/// [`type_a_return_states`] with what the template parameters stand for.
///
/// The substitution is applied to **the type of a parameter**, which is where a template parameter can appear in a
/// return statement: `return p;` gives `_Iter&&`, and at a call site with `_Iter = int*` that is `int*&&` — the answer
/// the `auto` variable's declaration is asking for. Everything else in this walk is about the expression's shape and
/// **The usual arithmetic conversions**, for the one question a fold expression asks: what `(a + b + c)` is.
///
/// Only the widening half is modelled — `int` with `double` is a `double`, `float` with `long double` a `long double` —
/// because that is what decides the type of a sum. A non-arithmetic operand answers `None`: `Widget + Widget` is an
/// `operator+` this layer would have to find, and answering `Widget` for a user-defined sum is the kind of confidence that
/// produces wrong types.
pub(crate) fn the_widest_of(types: &[crate::sema::types::Type]) -> Option<String> {
    let mut widest: Option<(u8, String)> = None;
    for a_type in types {
        let spelling = a_type.to_string();
        let rank = match spelling.trim() {
            "long double" => 6,
            "double" => 5,
            "float" => 4,
            "unsigned long long" | "long long" | "long long int" | "unsigned long long int" => 3,
            "unsigned long" | "long" | "long int" | "unsigned long int" | "signed long" => 2,
            "bool" | "char" | "signed char" | "unsigned char" | "short" | "unsigned short"
            | "int" | "unsigned" | "unsigned int" | "signed int" | "short int"
            | "unsigned short int" | "signed short" | "signed short int" | "size_t"
            | "std::size_t" | "ptrdiff_t" | "wchar_t" | "char8_t" | "char16_t" | "char32_t" => 1,
            _ => return None,
        };
        match &widest {
            Some((widest_rank, _)) if *widest_rank >= rank => {}
            _ => widest = Some((rank, spelling.trim().to_string())),
        }
    }
    // **Integral promotion**: a `char` or a `bool` summed is an `int`, and a list of them is not a `char`.
    let (rank, spelling) = widest?;
    if rank == 1 && spelling != "size_t" && spelling != "std::size_t" && spelling != "ptrdiff_t" {
        return Some("int".to_string());
    }
    Some(spelling)
}

/// **A data member's declared type**, read from the class body the function is written in.
///
/// ```cpp
/// class Box {
///     T value_;                                  // ← `T` for the name `value_`
/// public:
///     auto copy_of() const { return value_; }    // the return expression names it
/// };
/// ```
///
/// `None` for a name that is not a field of that class — a parameter (handled before this), a global, a member function
/// (a function has a parameter list, and its *type* is not what an accessor returns).
///
/// # Why the walk is shallow and the text is read last
///
/// This runs for **every** deduced return whose name is not a parameter, and a class of three hundred members would make the
/// walk over its own children the whole cost. So the declarator is taken as the member's *own child* rather than through
/// `descendants()`, nothing is allocated until a name has matched, and a class body larger than
/// [`MOST_MEMBERS_AN_ACCESSOR_CLASS_HAS`] is not searched at all — the accessors this exists for are in small classes, and a
/// larger one keeps the unknown return it had before this reader existed.
fn a_data_member_type(expression: &CppSyntaxNode, declaration: &CppSyntaxNode) -> Option<String> {
    use cpp_parser::{CppAstNode, CppClassBody, CppDeclaration, CppNameExpr};
    /// A cost bound, not a language rule — see the note above.
    const MOST_MEMBERS_AN_ACCESSOR_CLASS_HAS: usize = 64;

    let name = CppNameExpr::cast(expression.clone())?.get_name_text()?;
    // **The class body the function is written in**, and its members one level down: a member declaration's own typed accessor
    // answers the type it writes for the declarator named `name`, which is the field's type.
    let body = cpp_parser::typed_ancestors::<CppClassBody>(declaration).next()?;
    for (seen, member) in body.children::<CppDeclaration>().enumerate() {
        if seen >= MOST_MEMBERS_AN_ACCESSOR_CLASS_HAS {
            return None;
        }
        if let Some(the_type) = member.get_type_text_of(&name) {
            return Some(the_type);
        }
    }
    None
}

/// does not depend on what a name stands for.
/// The type a return statement states, with what the template parameters stand for substituted.
///
/// The same reading as [`type_a_return_states`], plus the two shapes a template body writes that a plain one does not: a **fold
/// expression** (`(values + ...)`), whose type is every argument the call passed widened by `+`, and a name that came from a
/// template parameter, which is substituted before it is answered.
fn type_a_return_states_with(
    expression: &cpp_parser::CppExpr,
    declaration: &CppSyntaxNode,
    substitutions: &crate::sema::types::TypeSubstitutions<'_>,
    argument_types: &[crate::sema::types::Type],
) -> Option<String> {
    use cpp_parser::{CppAstNode, CppExpr};
    match expression {
        // **A parenthesised expression is what it wraps.** A fold has to be written in parentheses — `(values + ...)` — so the
        // node a `return` hands here is the `ParenExpr`.
        CppExpr::ParenExpr(paren) => type_a_return_states_with(
            &paren.get_expr()?,
            declaration,
            substitutions,
            argument_types,
        ),
        CppExpr::NameExpr(name) => {
            // **A parameter, a data member of the class the function is in, a local of the same body, or an init-capture** —
            // the four names a `return` can write here.
            let written = parameter_type(name.syntax(), declaration)
                .or_else(|| a_data_member_type(name.syntax(), declaration))
                .or_else(|| a_local_variables_type(name.syntax(), declaration))
                .or_else(|| a_captures_type(name.syntax()))?;
            Some(substituted_spelling(&written, substitutions))
        }
        CppExpr::BinaryExpr(binary) => {
            // **A fold expression** — `(values + ...)`: its type is every argument the call passed, widened by `+`. Four
            // `int`s are an `int`, and `1` with `2.5` is a `double`. Before this the arm answered `None` for want of a
            // right-hand type, and `auto total = sum_of(1, 2, 3, 4)` stayed unknown.
            if binary.get_fold().is_some()
                && let Some(widest) = the_widest_of(argument_types)
            {
                return Some(widest);
            }
            if is_a_comparison(binary) {
                return Some("bool".to_string());
            }
            let left_type = type_a_return_states_with(
                &binary.get_lhs()?,
                declaration,
                substitutions,
                argument_types,
            )?;
            let right_type = type_a_return_states_with(
                &binary.get_rhs()?,
                declaration,
                substitutions,
                argument_types,
            )?;
            if left_type == right_type {
                return Some(left_type);
            }
            // **`X + 0` is `X`** — [expr.add]: the integer is a count and never part of the answer.
            if matches!(binary.get_operator_text().as_deref(), Some("+" | "-")) {
                if is_a_literal_zero(&binary.get_rhs()?) {
                    return Some(left_type);
                }
                if is_a_literal_zero(&binary.get_lhs()?) {
                    return Some(right_type);
                }
            }
            None
        }
        _ => type_a_return_states(expression, declaration),
    }
}

/// **The type a return statement states, when the syntax alone states one.**
///
/// Four shapes, and each is a *reading* rather than an inference — the type is written in the expression or in a declaration
/// beside it:
///
/// * **a cast**: `return static_cast<T>(x);` has the type `T`, whatever `x` is;
/// * **a construction**: `return std::array<int, Size>{};` is a prvalue of the type it names;
/// * **a literal**: `return 1;` is an `int`, and no declaration anywhere says otherwise;
/// * **a name**, which is a parameter, a data member, a local, or an init-capture;
/// * **an arithmetic pair whose operands agree**, which is C++'s usual arithmetic conversion in the one case that needs no
///   table, and `X + 0`'s own rule ([expr.add]).
///
/// Everything else is refused: `return f(x);` needs the callee's declaration, a member access needs the object's type, and two
/// operands that *disagree* need the conversion ranks this deliberately does not keep.
fn type_a_return_states(
    expression: &cpp_parser::CppExpr,
    declaration: &CppSyntaxNode,
) -> Option<String> {
    use cpp_parser::{CppAstNode, CppExpr};
    match expression {
        CppExpr::CastExpr(cast) => {
            let written = cast.get_type_id()?.get_type_text();
            (!written.is_empty()).then_some(written)
        }
        CppExpr::InitListExpr(list) => {
            let head = list.get_constructed_type()?;
            let written = cpp_parser::subtree_text_spaced(head.syntax());
            (!written.is_empty()).then_some(written)
        }
        // **A parenthesised construction is deliberately not here.** The tree cannot tell a constructor from a function call —
        // `Widget(1)` and `make()` are the same shape — and answering the callee's spelling for both read `return make();` as a
        // `make`. A braced list has no such twin: `T{…}` is a construction.
        CppExpr::LiteralExpr(literal) => {
            Some(literal_type(&literal.get_literal_text()?).to_string())
        }
        CppExpr::NameExpr(name) => parameter_type(name.syntax(), declaration)
            .or_else(|| a_data_member_type(name.syntax(), declaration))
            .or_else(|| a_local_variables_type(name.syntax(), declaration))
            .or_else(|| a_captures_type(name.syntax())),
        CppExpr::BinaryExpr(binary) => {
            if is_a_comparison(binary) {
                return Some("bool".to_string());
            }
            let left_type = type_a_return_states(&binary.get_lhs()?, declaration)?;
            let right_type = type_a_return_states(&binary.get_rhs()?, declaration)?;
            if left_type == right_type {
                return Some(left_type);
            }
            if matches!(binary.get_operator_text().as_deref(), Some("+" | "-")) {
                if is_a_literal_zero(&binary.get_rhs()?) {
                    return Some(left_type);
                }
                if is_a_literal_zero(&binary.get_lhs()?) {
                    return Some(right_type);
                }
            }
            None
        }
        _ => None,
    }
}

/// The type a literal spells, by the shape of its own text.
///
/// The suffixes are what separate the cases: `1` is `int` and `1u` is not, `1.5` is `double` and `1.5f` is not.
/// A string literal is **not** attempted — `"x"` is a `const char*` and `L"x"` is not — because the caller reads
/// this for an `auto` variable's type, where a wrong answer is worse than none.
fn literal_type(written: &str) -> &'static str {
    let trimmed = written.trim();
    if trimmed == "true" || trimmed == "false" {
        return "bool";
    }
    if trimmed == "nullptr" {
        return "nullptr_t";
    }
    if trimmed.starts_with('"') || trimmed.starts_with("'") {
        return "const char*";
    }
    let looks_like_a_float = trimmed.contains('.')
        || (trimmed.contains('e') || trimmed.contains('E')) && !trimmed.starts_with("0x");
    if looks_like_a_float {
        return "double";
    }
    "int"
}

/// The type of the **parameter** this name is, when it is one of `declaration`'s own parameters.
///
/// The parameter list is in the declaration the return statement belongs to, so this is a walk within one node
/// rather than a lookup: `auto f(int* _First) { return _First; }` says `int*` twice, and the second time nobody has
/// to be asked. `None` for any other name — a local, a global, a template parameter — because their types are
/// declared where this layer cannot reach without the scope tree.
fn parameter_type(name: &CppSyntaxNode, declaration: &CppSyntaxNode) -> Option<String> {
    use cpp_parser::{CppAstNode, CppNameExpr};
    let written = CppNameExpr::cast(name.clone())?.get_name_text()?;
    let parameter = the_parameters_of(declaration)
        .into_iter()
        .find(|parameter| parameter.get_name_text().as_deref() == Some(written.as_str()))?;
    let the_type = parameter.get_type_text();
    (!the_type.is_empty()).then_some(the_type)
}

/// **The type an init-capture's name has** — `[value = counter + 10] { return value; }` is an `int`.
///
/// The capture is written in the lambda's own capture list (`LambdaCapture "value = counter + 10"`), and its initializer is
/// an expression the same walk types — with the **enclosing** declaration's parameters in hand, because that is the scope the
/// initializer is written in (`counter` is a parameter of the function containing the lambda).
///
/// A plain capture (`[counter]`, `[&counter]`) has no initializer and answers nothing here: its type is the enclosing
/// variable's, which is a name lookup this layer does not make from a capture node.
fn a_captures_type(expression: &CppSyntaxNode) -> Option<String> {
    use cpp_parser::{CppAstNode, CppDeclaration, CppLambdaExpr, CppNameExpr};
    let written = CppNameExpr::cast(expression.clone())?.get_name_text()?;
    // **The lambda the name is written in**, and the declaration around it that the initializer is read in: the innermost
    // lambda owns the capture list, while the initializer names what the *enclosing* function declares.
    let lambda = cpp_parser::typed_ancestors::<CppLambdaExpr>(expression).next()?;
    let capture = lambda
        .get_captures()?
        .get_captures()
        .find(|capture| capture.get_name_text().as_deref() == Some(written.as_str()))?;
    let initializer = capture.get_initializer()?;
    // **The outermost declaration around the lambda** — the initializer is written in the function's scope, not in the
    // variable the lambda is stored in (`auto initialised = [value = …]` is the nearest declaration and holds no `counter`).
    let enclosing = cpp_parser::typed_ancestors::<CppDeclaration>(lambda.syntax()).last()?;
    type_a_return_states_with(
        &initializer,
        enclosing.syntax(),
        &crate::sema::types::TypeSubstitutions::new(&[], &[]),
        &[],
    )
}

/// **The type a local variable's own declaration writes** — `std::vector<int> result;` answers `std::vector<int>`.
///
/// The sibling of [`parameter_type`], and it exists for the same shape of hole: `auto pipeline() { std::vector<int>
/// result; … return result; }` returns a **local**, and the deduction knew a parameter and a data member only — so
/// `pipeline()` had no return type and a range-for over its result had no element (measured on `ranges.cpp`).
///
/// The declaration is the last one **before the return** that declares the name, which is the one C++ would read there: a
/// nested block's shadow comes later in document order, and taking the last rather than the first is what makes that true.
/// The type is the declaration's text with the name taken out, the same rule [`parameter_type`] uses — and `rfind`, because
/// `int int_` has the name inside the type's spelling as well.
fn a_local_variables_type(
    expression: &CppSyntaxNode,
    declaration: &CppSyntaxNode,
) -> Option<String> {
    use cpp_parser::{CppAstNode, CppNameExpr};
    let name = CppNameExpr::cast(expression.clone())?;
    let written = name.get_name_text()?;
    // **The declarations before this point that declare this name and are still in scope** — the last one wins, which is what
    // a nested block's shadow means. The declaration's own typed accessor answers the type it writes for that declarator, so
    // `int* a, b;` gives `int*` for `a` and `int` for `b`.
    let mut found: Option<String> = None;
    for candidate in the_declarations_in(declaration) {
        if candidate.get_range().end() > name.get_range().start() {
            continue;
        }
        if !a_block_statement_encloses(candidate.syntax(), name.syntax()) {
            continue;
        }
        if let Some(the_type) = candidate.get_type_text_of(&written) {
            found = Some(the_type);
        }
    }
    found
}

/// **Is this declaration in a block that encloses the expression?**
///
/// A local of a **sibling** block is written before the expression and is not in scope at it, which a position filter alone
/// cannot see: `if (a) { int n = 1; } return n;` names nothing. A declaration with no enclosing block (a global, a class member)
/// has no block to be inside and is left to the caller's other questions.
fn a_block_statement_encloses(declaration: &CppSyntaxNode, expression: &CppSyntaxNode) -> bool {
    use cpp_parser::{CppAstNode, CppCompoundStat};
    let Some(block) = cpp_parser::typed_ancestors::<CppCompoundStat>(declaration).next() else {
        return true;
    };
    block.get_range().contains_range(expression.text_range())
}

/// [`declared_returns_of`] for a caller that has the file's shapes already.
///
/// The whole of that function except the shapes: the written return type first, and the deduced one — which needs the
/// tree rather than the table — only for a function that writes none. The split is what makes a caller with many
/// declarations to read pass its table once; see [`crate::index::project`]'s `fact_from_binding_with`.
pub(crate) fn declared_returns_of_in(
    shapes: &DeclarationShapes,
    root: &CppSyntaxNode,
    binding: &Binding,
) -> Option<String> {
    let written = declared_returns_of_with(shapes, binding);
    if let Some(spelling) = written {
        // **An operator's return type, when the fact's readers can resolve the spelling.** See
        // [`an_operator_return_is_usable`]: `std::vector<Token>::operator[]` says `reference`, a typedef inside the
        // class, and a fact that carried it typed `tokens[i]` as a class called `reference`.
        if binding.kind != BindingKind::OperatorFunction
            || an_operator_return_is_usable(root, binding, &spelling)
        {
            return Some(spelling);
        }
        return deduced_returns_of(root, binding);
    }
    deduced_returns_of(root, binding)
}

/// **May an operator's return spelling be stored in a fact?**
///
/// A fact is read by code that has the declaration and *not* the class's scope, so an unqualified name in it may mean
/// something else than it does in the class body: `reference` is `std::vector`'s own typedef, and a fact that carried
/// it typed `tokens[i]` as a class called `reference` — 60 false positives over four files, measured.
///
/// Two kinds of spelling mean the same thing everywhere and are always usable: a **builtin** (`int`, `bool`,
/// `void*`) and a **qualified** name (`std::string`, `::Widget`). Anything else is usable only when the class body
/// that declares the operator does **not** declare a name of its own with that spelling — which is the same rule as
/// the language's (an unqualified name in a member's return type is looked up in the class first), asked of the tree
/// instead of the scope table because this reader is handed the file's tree.
fn an_operator_return_is_usable(root: &CppSyntaxNode, binding: &Binding, spelling: &str) -> bool {
    fn means_the_same_everywhere(kind: &crate::sema::types::Type) -> bool {
        use crate::sema::types::Type;
        match kind {
            Type::Builtin { .. } => true,
            Type::Named { name, arguments } => {
                name.contains("::") && arguments.iter().all(means_the_same_everywhere)
            }
            Type::Pointer { to }
            | Type::Qualified { of: to }
            | Type::Reference { to, .. }
            | Type::Array { of: to, .. } => means_the_same_everywhere(to),
            _ => false,
        }
    }

    if means_the_same_everywhere(&crate::sema::types::parse_type_spelling(spelling)) {
        return true;
    }
    let Some(head) = crate::sema::types::parse_type_spelling(spelling)
        .class_name()
        .map(str::to_string)
    else {
        return true;
    };
    !the_enclosing_body_declares(root, binding, &head)
}

/// **Does the class body that declares `binding` declare `name` itself?**
///
/// The question [`an_operator_return_is_usable`] turns on: a member's return type is looked up in its class before
/// anything outside, so a name the body declares is not a name the fact can carry. Walked from the declaration up to
/// the nearest `ClassBody`, then over that body's own declarations — one subtree, not the file.
fn the_enclosing_body_declares(root: &CppSyntaxNode, binding: &Binding, name: &str) -> bool {
    use cpp_parser::CppAstNode;
    let Some(declaration) = declaration_of(root, binding) else {
        return false;
    };
    let Some(body) = cpp_parser::typed_ancestors::<cpp_parser::CppClassBody>(&declaration).next()
    else {
        return false;
    };
    // **An alias, and the name it introduces** — that is the case this gate exists for, and it is the only one read.
    //
    // The first version of this walked every declaration in the body and matched the word anywhere in its
    // declarator, and that was wrong in the direction that costs semantics: `Money operator+(const Money& other)`
    // mentions `Money` in its *parameter*, so a class whose operator returns its own name looked as if the body
    // declared one — the return type was dropped and `a + a` answered nothing.
    //
    // So the shape is read: a declaration whose text starts with `using`/`typedef` introduces a name, and for
    // `using X = …` it is the identifier right after `using` while for `typedef … X;` it is the last identifier
    // before the `;`. A nested class or enum of the same name is **not** covered — its declaration names the
    // enclosing scope's entity differently in this tree — and that is a known gap rather than a claim: an operator
    // returning `Widget` where the body declares `struct Widget` keeps the spelling, and a fact's reader resolves it
    // to the class it names, which is the same class.
    // **An alias, and the name it introduces** — `using X = …` and `typedef … X;` both expose `X` as the declaration's name, and
    // a nested class of the same name is deliberately not covered (see the note above). Asked of the declarations' names rather
    // than of their text.
    body.syntax()
        .descendants()
        .filter_map(cpp_parser::CppDeclaration::cast)
        .filter(|declaration| {
            matches!(
                CppSyntaxKind::from(declaration.syntax().kind()),
                CppSyntaxKind::UsingDecl | CppSyntaxKind::TypedefDecl
            )
        })
        .any(|declaration| {
            declaration
                .get_name()
                .is_some_and(|introduced| introduced.get_name_text() == name)
        })
}

pub(crate) fn declared_returns_of_with(
    shapes: &DeclarationShapes,
    binding: &Binding,
) -> Option<String> {
    // **A `Function`, and an operator when the spelling cannot be a name from the class's own body.**
    //
    // An operator does declare a return type, and recording every one of them was measured and rejected once:
    // `std::vector<Token>::operator[]` says `reference`, and `reference` is a *typedef inside the class* — so the
    // fact carried the unsubstituted name, `tokens[i]` typed as a class called `reference`, and `undefined-field`
    // reported **60 false positives over four files** of a real project (`tokens[index]` has no member `TokenType`).
    //
    // That measurement is about a *lookup* an unqualified name needs, not about operators: `int`, `bool` and
    // `std::string` mean the same thing in a fact as in the class body, and `Money operator+` names a class from
    // outside the body. So an operator records its return type here and [`an_operator_return_is_usable`] decides
    // whether the *fact* may carry it — the typedef case is refused there, with the file's tree in hand.
    // **A conversion operator's return type is its own name.** `operator bool` converts *to* `bool`, and what is
    // written before it is a specifier — often through a macro. libstdc++ writes
    //
    // ```text
    //     _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR
    //     operator bool() const
    // ```
    //
    // and the written reading recorded `_GLIBCXX20_CONSTEXPR` as the return type — a macro's *name*, which no lookup
    // can resolve and which is not a type at all (measured: `facts_vs_rendering` over three headers, the one
    // contradiction left after the branch-guarded declarations were accounted for).
    // The rule is the **spelling**, not the kind: the walker classifies `operator bool` as an operator, and a
    // conversion operator is the one operator whose name carries a space and a type (`operator[]` and `operator+` do
    // not match this pattern, and their return type is a real specifier).
    let spelled = binding.name.text();
    if let Some(target) = spelled.trim().strip_prefix("operator ").map(str::trim)
        && !target.is_empty()
    {
        return Some(target.to_string());
    }

    if !matches!(
        binding.kind,
        BindingKind::Function | BindingKind::OperatorFunction
    ) {
        return None;
    }

    // Three things are collected on the way past the declaration, and **the direction each is kept in is not
    // decoration**: the specifier sequence and the trailing return type are the innermost on the path ("the last
    // one seen going down"), while the declarator is the outermost — its text *before the name* is where the
    // operators in front of it live, and the nested declarators are its pointer parts.
    let mut specifiers = None;
    let mut trailing = None;
    let mut declarator = None;
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        if let Some(node) = &shape.specifiers {
            specifiers = Some(node.clone());
        }
        if let Some(node) = &shape.trailing
            && let Some(type_id) = cpp_parser::typed_children::<cpp_parser::CppTypeId>(node).next()
        {
            let spelling = type_id.get_type_text().trim().to_string();
            if !spelling.is_empty() {
                trailing = Some(spelling);
            }
        }
        // The **outermost** declarator on the path, which is the one whose own text holds the name: the nested
        // ones are its pointer/reference parts, and reading the text before the name off the outer one gets every
        // operator in order.
        if declarator.is_none()
            && let Some(node) = &shape.declarator
        {
            declarator = Some(node.clone());
        }
    });

    if let Some(trailing) = trailing {
        let spelling = trailing.trim().to_string();
        return (!spelling.is_empty()).then_some(spelling);
    }

    // **The operators written before the name are part of the return type**: `Widget* make()`, `const Widget&
    // get()`. They live in the *declarator*, so the specifier sequence does not have them, and reading only the
    // specifiers answered `Widget` for `Widget* make()` — a return type with no pointer on it. Everything that
    // follows a return type reads that spelling, so `(*make()).size` looked like a member lookup on a value and
    // `make()->size` was unanswerable.
    //
    // Only the operators, and only when they are *purely* operators: `int (*f)(int)` returns a function pointer
    // and its text before the name is `(*` — a spelling this cannot assemble, so it keeps the specifiers instead
    // of returning something that is not a type.
    let before_the_name = declarator
        .as_ref()
        .map(|declarator| {
            before(
                &declarator.text().to_string(),
                declarator,
                binding.name_range,
            )
        })
        .filter(|operators| {
            operators
                .chars()
                .all(|c| c == '*' || c == '&' || c.is_whitespace())
        })
        .unwrap_or_default();

    // **The specifiers are read as syntax, not as text**, which is the rule [`crate::sema::types::read_specifiers`]
    // states and the one that already fixed `std::cin`: a macro standing where a specifier goes is a *name* to the
    // grammar, no C++ type is spelled as two unqualified names in a row, so **the last name wins** — and everything
    // that is not a type's name (attributes, `constexpr`, storage class) is dropped by node kind rather than by
    // being on a list of keywords.
    //
    // What the text-based rule cost, measured on MSVC's `<xstring>`: `_NODISCARD _CONSTEXPR20 reference back()`
    // recorded `_NODISCARD _CONSTEXPR20 reference` as the return type, because neither macro is in
    // `DECLARATION_SPECIFIERS`. Nothing can resolve that spelling — the macros are the library's, the name is last —
    // so `auto y = x.back()` answered with it, and `y` had no type at all. Read as syntax the same declaration says
    // `reference`, which is a member alias the substitution step can follow (`typedef _Ty& reference` with
    // `_Ty = char` → `char&`).
    //
    // An **empty** specifier sequence is still `None` rather than `void`: a constructor spells no return type, and
    // the node-based reader's fallback (`void`) is the right answer for "this declaration wrote nothing as a type"
    // only when there is a declaration — see the `None` half of this function's contract.
    let specifiers = specifiers?;
    // **The declarator's own qualified name is not a return type**, for the same reason it is not a variable's type —
    // `std::size_t LuaSource::GetLine(std::size_t offset)` recorded `returns: Some("LuaSource::GetLine")`, and a hover
    // showed that as the function's answer where the file wrote `std::size_t`. See
    // [`crate::sema::types::type_of_declaration_named`].
    let declared = binding.name.text();
    let spelling = if specifiers.text().is_empty() {
        String::new()
    } else {
        crate::sema::types::read_specifiers_named(
            &specifiers,
            Some(declared.as_str()),
            cpp_parser::source_range(specifiers.text_range()),
        )
        .to_string()
    };

    let spelling = format!("{spelling} {before_the_name}");
    let spelling = spelling.split_whitespace().collect::<Vec<_>>().join(" ");

    // A **deduced** return type is not a type this layer can name. `auto` and `decltype(auto)` are the two
    // spellings, and both would otherwise be looked up as class names — answering "no member `size` in `auto`"
    // where the honest answer is that the file never said.
    if spelling == "auto" || spelling == "decltype(auto)" {
        return None;
    }

    (!spelling.is_empty()).then_some(spelling)
}

/// `text` up to the span `name` occupies — the mirror of [`without`], which cuts the name *out*.
///
/// Both are arithmetic on byte offsets into the same source rather than a search for a word, for the reason
/// [`without`] gives: `Widget Widget(1);` has two spellings of one name, and a search cuts the wrong one.
fn before(text: &str, node: &CppSyntaxNode, name: cpp_parser::SourceRange) -> String {
    if name.length == 0 {
        return String::new();
    }

    let start = usize::from(node.text_range().start());
    let to = name.start_offset.saturating_sub(start);

    if to > text.len() || !text.is_char_boundary(to) {
        return String::new();
    }

    text[..to].trim().to_string()
}

/// **Who may name a declaration written at `offset`** — the access level in force there, or `None` outside every
/// class body.
///
/// The tree-side twin of [`DeclarationShapes::access_at`], for the one producer that has no shapes table: a fact
/// built from the **buffer's** scope tree ([`crate::index::project::fact_from_binding`]) is handed a node and a
/// binding, and a completion after `.` on the buffer's own class is exactly where this answer is shown.
///
/// The walk is up from the name to the innermost class body containing it, then along that body's own children for
/// the **last** access label before the name — the level in force. The class key decides the default before any
/// label: `struct` and `union` are public, `class` is private.
pub fn declared_access_at(root: &CppSyntaxNode, offset: usize) -> Option<Access> {
    let token = cpp_parser::token_at(root, offset)?;

    use cpp_parser::{
        CppAccessSpecifier, CppAstNode, CppClassBody, CppClassDef, CppKind, CppTokenKind,
    };
    let body = cpp_parser::typed_ancestors::<CppClassBody>(&token.parent()?).next()?;

    // **The class key decides the default before any label**: `struct` and `union` are public, `class` is private.
    let default =
        body.syntax()
            .parent()
            .and_then(CppClassDef::cast)
            .map_or(Access::Private, |class| {
                if class.is_struct() || class.is_union() {
                    Access::Public
                } else {
                    Access::Private
                }
            });

    let mut level = default;
    for child in body.syntax().children() {
        // Only the labels **before** the name are in force: a `private:` written below it does not change what this
        // declaration is.
        if usize::from(child.text_range().start()) >= offset {
            break;
        }
        let Some(label) = CppAccessSpecifier::cast(child) else {
            continue;
        };
        let Some(keyword) = label.get_keyword() else {
            continue;
        };
        level = match keyword.get_keyword_kind() {
            CppKind::Token(CppTokenKind::PublicKeyword) => Access::Public,
            CppKind::Token(CppTokenKind::ProtectedKeyword) => Access::Protected,
            CppKind::Token(CppTokenKind::PrivateKeyword) => Access::Private,
            _ => continue,
        };
    }

    Some(level)
}

/// **Is a declaration written at `offset` exported** from the module the file declares?
///
/// The tree-side twin of [`DeclarationShapes::exported_at`], for the producer that has no shapes table — see
/// [`declared_access_at`], whose shape this shares.
///
/// The rule is [`begins_an_exported_region`], asked of every node the declaration is written inside: `export`
/// itself, an `export { … }` block, or an exported namespace all govern what is written within them.
pub fn declared_exported_at(root: &CppSyntaxNode, offset: usize) -> bool {
    let Some(token) = cpp_parser::token_at(root, offset) else {
        return false;
    };

    token
        .parent_ancestors()
        .any(|node| begins_an_exported_region(&node))
}

/// **Does `export` govern what is written inside this node?**
///
/// Two spellings, because the grammar writes the keyword in two places, and reading only one of them is wrong in a
/// way that hides a whole library:
///
/// ```text
/// export int f();            the keyword is the *first token* of the declaration
/// export { int f(); }        …and of the export block
/// export namespace n { … }   the keyword is a **sibling** of the namespace, not inside it
/// ```
///
/// Measured on the module fixture (`tests/fixtures/modules`), whose interface unit is
/// `export module mathlib; export namespace mathlib { … }`: with only the first rule every member of an exported
/// namespace came out unexported, so a file that says `import mathlib;` could name **none** of them — the answer
/// went from "one name too many" to "nothing at all".
///
/// Trivia between the keyword and the node is skipped: `export` and the declaration it exports may be separated by
/// a newline, a comment, or both.
///
/// # The file itself is not a region, and that is not a detail
///
/// A file whose **first** declaration is `export module m;` begins with the `export` token — so a rule that asked
/// the first-token question of every ancestor answered `true` for every declaration in the file, exported or not.
/// Measured on the four-line fixture in `a_declaration_is_exported_only_where_export_reaches_it`: all four names,
/// including one below a `namespace` with no `export` anywhere above it, came out exported; and on the real module
/// fixture (`tests/fixtures/modules`, which *does* start with `export module mathlib;`) it would have offered
/// `hidden_helper` again — the very name this whole reading exists to hide. `export` heads a **declaration**; a
/// translation unit is not one.
fn begins_an_exported_region(node: &CppSyntaxNode) -> bool {
    if node.parent().is_none() {
        return false;
    }

    // **The first significant token**, because `export` may follow a comment or a newline: the parser's own helper is the
    // one that skips trivia, so the question is a token kind and not a search.
    let starts_with_export = cpp_parser::first_significant_token(node).is_some_and(|token| {
        cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::ExportKeyword
    });

    if starts_with_export {
        return true;
    }

    let mut previous = node.prev_sibling_or_token();
    while let Some(element) = previous {
        if let Some(token) = element.as_token() {
            if cpp_parser::is_trivia(cpp_parser::CppTokenKind::from(token.kind())) {
                previous = token.prev_sibling_or_token();
                continue;
            }

            return cpp_parser::CppTokenKind::from(token.kind())
                == cpp_parser::CppTokenKind::ExportKeyword;
        }

        // A *node* before this one: the keyword is not there, whatever it is.
        return false;
    }

    false
}

/// **The parameter list a declaration at `offset` was written with**, as the file spells it — parentheses included.
///
/// The **innermost `Declarator` ancestor** of the name, and *its* `ParameterList` child, which is the same reading
/// [`crate::inlay::parameter_list_of`] makes for a call's parameters: a variable declared inside a function body has
/// the enclosing function's list above it, and `void (*f(int a))(int b)` has two lists in one declaration. The
/// declarator the name is a name *of* is the one whose list says what the name's parameters are.
///
/// `None` for a declaration that has no such list (every variable, class and alias) and for a name the tree does not
/// hold a token for.
pub fn parameter_list_at(root: &CppSyntaxNode, offset: usize) -> Option<String> {
    let token = cpp_parser::token_at(root, offset)?;

    use cpp_parser::CppAstNode;
    // **The innermost `Declarator` ancestor** of the name, whose `ParameterList` child says what the name's parameters are.
    let declarator =
        cpp_parser::typed_ancestors::<cpp_parser::CppDeclarator>(&token.parent()?).next()?;

    crate::inlay::parameter_list_text(declarator.syntax())
}

/// **The parameter list as the file spells it, read from the shape table** — `"(int a, int b)"`.
///
/// [`parameter_list_at`] answers the same question from a root and an offset: it asks the tree for the token at the
/// offset and walks the ancestor chain until it meets a `Declarator`. That is a walk per **declaration**, and the
/// `Declarator` it is looking for is already recorded on the shape ([`Shape::declarator`], collected in the pass
/// that builds the table) — so the walk is asking the tree a question the table has already answered.
///
/// Measured together with [`enclosing_namespaces_of`]: the two were **1576 ms of a 4562 ms `Facts` stage** over 126
/// files of MSVC's standard library.
pub(crate) fn declared_parameters_with(
    shapes: &DeclarationShapes,
    binding: &Binding,
) -> Option<String> {
    let mut found = None;
    shapes.on_the_path(binding.name_range.start_offset, |shape| {
        // **The declarator that names this binding**, not merely the last one on the path.
        //
        // A path can hold several declarators, and the *innermost* one is often a prefix rather than the
        // declaration: `__ostream_type& operator<<(long)` parses as
        //
        // ```text
        // InitDeclarator
        //   Declarator  &  operator<<(long)      <- the name and the list are here
        //     Declarator  &                      <- and this is the last one on the path
        //     NameExpr    operator<<
        //     ParameterList (long)
        // ```
        //
        // Taking the last declarator answered "no parameter list" for every operator and every function whose
        // return type ends in `&` or `*`. Measured on libstdc++ with `parse_loss_probe`: **1 937 of 15 253**
        // function facts had `parameter_list: None`, and `bits/ostream.h` was 53 of 66 — a signature help, a
        // hover and an argument check that all see a function with no arguments.
        if let Some(declarator) = &shape.declarator
            && names_the_binding(declarator, binding.name_range.start_offset)
        {
            found = Some(declarator.clone());
        }
    });

    crate::inlay::parameter_list_text(&found?)
}

/// Is the **name this declarator declares** the one at `offset`?
///
/// A declarator's own name is a **direct child** — the parameter types are names too, but they are deeper — so the
/// question is "does one of my direct children name the binding", which is what separates the declarator that
/// declares from the wrappers around it (`&`, `*`, `const`) and from a function pointer's
/// `Declarator > (Declarator(*f), ParameterList)`, whose name is not a direct child precisely because the list
/// there belongs to the pointer's type rather than to a function.
fn names_the_binding(declarator: &CppSyntaxNode, offset: usize) -> bool {
    use cpp_parser::CppAstNode;
    let at = u32::try_from(offset).unwrap_or(u32::MAX);
    cpp_parser::typed_children::<cpp_parser::CppNameExpr>(declarator).any(|name| {
        let range = name.get_range();
        u32::from(range.start()) <= at && at < u32::from(range.end())
    })
}

/// The names a class template declares its parameters with** — `["_Ty", "_Alloc"]` for `std::vector`.
///
/// Empty for anything that is not a class template, which includes an ordinary class *and* a partial
/// specialization: `template <class T> struct vector<T*>` declares parameters of its own, and a caller that took
/// them for the primary template's would substitute the wrong argument into the wrong place.
///
/// # Why the walk goes through the template declaration
///
/// Because that is where the list is: `template <…>` is a `TemplateDecl` and the class it introduces is a
/// *sibling* of it rather than a child, so the list, the class body and the binding are three nodes whose only
/// common ancestor is the declaration they share. The walk asks it from the top and keeps the one whose specifier
/// sequence contains the binding — which also gives the partial specializations away, since a file with two of them
/// has two template declarations and the offsets are what tell them apart. Where two of them contain the binding —
/// a class template inside a class template — the **innermost** is the one that introduces it.
///
/// Both obvious readings of *that* are wrong, and both were measured: searching the template's **descendants**
/// finds the `DeclSpecifierSeq` inside a `TemplateParameter` (`class _Ty` spells one) and decides the template
/// introduces whatever is being asked about; searching its **children** finds only the parameter list, so no
/// template ever introduces anything.
///
/// # Why the answer comes out of the shapes
///
/// Because a walk from the top is **per class binding**, and the tree it walks is the file's. That is affordable
/// for one file and is not for a **unit** read, where the tree is the whole program: on the 138-file project the
/// unit read spent **8.9 s of its 9.1 s `Facts`** here — the walk was over 332 755 tokens for every one of
/// thousands of class bindings, while the four questions beside it cost 0.2 s between them. The relation is the
/// same one either way, so it is read once, in the pass [`DeclarationShapes::of`] already makes, and the question
/// becomes a scan of the file's template declarations — a handful per file, and the same answer as before,
/// including for a partial specialization.
///
/// A pass over the file's template declarations **per class**, not per declaration: it is called once for each kind
/// of thing a class fact records, and a file has a handful of templates rather than a handful of thousands.
pub fn declared_template_parameters_of(root: &CppSyntaxNode, binding: &Binding) -> Vec<String> {
    declared_template_parameters_with(&DeclarationShapes::of(root), binding)
}

/// **What each template parameter of this declaration defaults to** — see [`declared_parameter_defaults_with`].
///
/// The tree-side twin of [`DeclFact::parameter_defaults`], for a declaration in the file being edited: a use that
/// writes fewer arguments than the declaration has parameters is answered by the same rule in both halves, and the
/// rule needs the defaults in both.
pub fn declared_parameter_defaults_of(root: &CppSyntaxNode, binding: &Binding) -> Vec<String> {
    declared_parameter_defaults_with(&DeclarationShapes::of(root), binding)
}

/// [`declared_template_parameters_of`] for a caller that has the file's shapes already.
///
/// The **innermost** template whose introduced specifier sequence contains the name wins, and that is a deliberate
/// change in the answer rather than in the cost: a class template nested in another one declares its own parameters
/// (`template <class _Ty> struct outer { template <class _Uty> struct inner { _Uty u; }; };`), both specifier
/// sequences contain a member of `inner`, and the outer list pairs the wrong argument with the wrong name. See
/// `a_nested_class_template_declares_its_own_parameters`, which is the case that was answered `["_Ty"]` before.
///
/// The walk from the top could not tell the two apart: it returned the **first** template it reached, which is the
/// outermost. The table is in document order, so the last entry that contains the name is the one that introduces
/// it.
pub(crate) fn declared_template_parameters_with(
    shapes: &DeclarationShapes,
    binding: &Binding,
) -> Vec<String> {
    // **A function template's list is as real as a class template's, and it was being thrown away.**
    //
    // This refused every binding that was not a class, so `template <class _Size_type, class _Unsigned_type>
    // constexpr _Size_type _Convert_size(const _Unsigned_type)` recorded `returns: Some("_Size_type")` and
    // `parameters: []` — the return type written with a name that nothing said the meaning of. A caller could then
    // read the return type and had no way to pair it with an argument, which is why `_Convert_size<size_type>(_Len)`
    // and every other call of a function template came back `UnknownType`.
    //
    // Measured on MSVC's `<vector>`/`<xutility>`: 11 `auto` refusals were calls with an **explicit** template
    // argument — `_Convert_size<size_type>(_Length)` — where the answer is a substitution and nothing else.
    //
    // The walk below never was class-specific: it finds the innermost `template <…>` whose introduced range contains
    // the name, which is as true of `template <class T> T f()` as of `template <class T> struct S`. The kinds are
    // the ones [`parameter_list_at`] is read for, so a declaration has either both or neither.
    if !matches!(
        binding.kind,
        BindingKind::Class
            | BindingKind::Function
            | BindingKind::Constructor
            | BindingKind::Destructor
            | BindingKind::ConversionFunction
            | BindingKind::OperatorFunction
            | BindingKind::LiteralOperator
            // **A variable template's list is as real as the others.** `template <class T> constexpr T pi = …` is a
            // `Variable` whose name sits in the specifier sequence the template introduces, exactly as a class's or a
            // function's does — and the filter above refused it, so `pi<double>` had no parameters to substitute into
            // and its type stayed the parameter's own name. Measured on the fixture this was written for:
            // `double x = pi<double>;` answered `T`.
            //
            // `Alias` and `Typedef` come along for the same reason (`template <class T> using ptr = T*;`), and they
            // are harmless for the aliases that have no template at all — a namespace alias introduces no parameter
            // list, so the scan below finds no template naming it and answers empty, which is what it answered before.
            | BindingKind::Variable
            | BindingKind::Alias
            | BindingKind::Typedef
    ) {
        return Vec::new();
    }

    let at = binding.name_range.start_offset;

    shapes
        .templates
        .iter()
        .rev()
        .find(|(introduced, _)| introduced.start_offset <= at && at <= introduced.end_offset())
        .map(|(_, list)| crate::sema::types::template_parameter_names(list))
        .unwrap_or_default()
}
/// **The constraint that applies to each *parameter*** — `["std::integral", ""]` for
/// `template <std::integral T> T twice(T a, int n)`.
///
/// One entry per parameter of the declaration, because that is the pairing a call site asks about ("does this
/// argument satisfy what *its* parameter was constrained by"). It is decided here, once, from the tree — so a file's
/// own answer and the index's answer cannot disagree, and no consumer has to re-derive the pairing by matching names
/// in raw text.
fn declared_constraints_with(shapes: &DeclarationShapes, binding: &Binding) -> Vec<String> {
    let at = binding.name_range.start_offset;

    let Some((_, head)) = shapes
        .templates
        .iter()
        .rev()
        .find(|(introduced, _)| introduced.start_offset <= at && at <= introduced.end_offset())
    else {
        return Vec::new();
    };
    let constrained: Vec<(String, String)> = crate::sema::types::template_parameters_in(head)
        .into_iter()
        .filter(|(_, constraint)| !constraint.is_empty())
        .collect();
    if constrained.is_empty() {
        return Vec::new();
    }

    let mut found = None;
    shapes.on_the_path(at, |shape| {
        if let Some(declarator) = &shape.declarator {
            found = Some(declarator.clone());
        }
    });
    let Some(declarator) = found else {
        return Vec::new();
    };

    use cpp_parser::{CppAstNode, CppDeclarator};
    let Some(declarator) = CppDeclarator::cast(declarator) else {
        return Vec::new();
    };
    let Some(list) = declarator.get_param_list() else {
        return Vec::new();
    };

    let mut per_parameter = Vec::new();
    for parameter in list.get_params() {
        // **The type is the template parameter**, and the question is asked of the type's *name node*: `const T& a` and `T a`
        // write the name `T`, while `T::value_type a` writes a qualified name — a different one — and `Total` was never `T`
        // under any comparison but a substring's.
        let written = parameter
            .get_decl_specifiers()
            .and_then(|specifiers| specifiers.get_type_name())
            .and_then(|name| name.get_name_text());
        let constraint = constrained
            .iter()
            .find(|(name, _)| written.as_deref() == Some(name.as_str()))
            .map(|(_, constraint)| constraint.clone())
            .unwrap_or_default();
        per_parameter.push(constraint);
    }

    per_parameter
}
/// The base classes a class-like declaration was written with, in declaration order.
///
/// Public for the same reason [`declared_type_of`] is: the query layer needs it for a class in the file being
/// edited, and a second implementation would be a second answer to "what does `class D : public B` inherit from".
///
/// The `BaseSpecifier` children of the class definition, each read as the text of its **name node** — so
/// `public B`, `private ns::C` and `virtual Base<int>` come back as `B`, `ns::C` and `Base<int>`. Taking the whole
/// specifier's text instead would read the access keyword as part of the base's name.
///
/// Empty for anything that is not a class, and for a class with no bases.
pub fn declared_bases_of(root: &CppSyntaxNode, binding: &Binding) -> Vec<String> {
    declared_bases_of_with(&DeclarationShapes::of(root), binding)
}

/// [`declared_bases_of`] for a caller that has the file's shapes already.
pub(crate) fn declared_bases_of_with(shapes: &DeclarationShapes, binding: &Binding) -> Vec<String> {
    use cpp_parser::CppAstNode;
    if binding.kind != BindingKind::Class {
        return Vec::new();
    }

    // The class-like definition the binding is inside — "the last one passed on the way down", which is the walk
    // [`declared_type_of`] makes and for the same reason: a binding's range does not cover the construct that
    // declared it, so the *tree* is asked where the declaration is rather than the geometry of a range.
    let mut owner = None;
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        // **A class-like definition**: `class`/`struct`/`union` are one typed node, and an enum writes bases too (its
        // underlying type).
        if cpp_parser::CppClassDef::can_cast(shape.kind) || shape.kind == CppSyntaxKind::EnumDef {
            owner = Some(shape.bases.clone());
        }
    });

    let Some(bases) = owner else {
        return Vec::new();
    };

    bases
        .iter()
        // **As written**: a base clause is a spelling a reader sees, and `conditional_t<true, Other<_Alloc>, Base>` keeps its
        // spacing — `get_qualified_name` drops the spaces after commas, which is a *name* spelling rather than a clause's.
        .map(cpp_parser::subtree_text_spaced)
        .filter(|name| !name.is_empty())
        .collect()
}

/// The specifier keywords that can precede a type without being part of it.
///
/// Deliberately *not* in this list: `unsigned`, `signed`, `long`, `short`. Those four *are* the type: stripping
/// `unsigned` from `unsigned long` leaves a different type.
///
/// `struct`, `class`, `union` and `enum` **are** in it, and the first version of this list had them out — on the
/// reasoning that "stripping `struct` from `struct Widget` would leave nothing to look up". It leaves `Widget`,
/// which is the only spelling anything looks up (`base_type_name` strips the same four keywords from every
/// receiver): an elaborated keyword says *what kind of* name follows, not what the name is. Measured, keeping them
/// cost `typedef struct exarg exarg_T;` its alias target — the target came out as `struct exarg`, a spelling no
/// scope has, so `exarg_T` answered an empty member list while `struct exarg`'s thirty members sat in the index
/// under `exarg`.
const DECLARATION_SPECIFIERS: &[&str] = &[
    "static",
    "extern",
    "inline",
    "virtual",
    "explicit",
    "friend",
    "mutable",
    "register",
    "thread_local",
    "constexpr",
    "consteval",
    "constinit",
    "typedef",
    "const",
    "volatile",
    "struct",
    "class",
    "union",
    "enum",
];

/// The type a specifier sequence spells: its own text, or — when it holds **several names** — from the last one on.
///
/// Remove declaration specifiers from the front of a type spelling, and trim what is left.
fn strip_declaration_specifiers(spelling: &str) -> String {
    let mut rest = spelling.trim();

    loop {
        let Some(word) = rest.split_whitespace().next() else {
            return String::new();
        };

        if !DECLARATION_SPECIFIERS.contains(&word) || word.len() == rest.len() {
            return rest.trim().to_string();
        }

        rest = rest[word.len()..].trim_start();
    }
}

/// One fact, or `None` for a binding the index has no use for.
///
/// A free function rather than a method so that the borrow of the fact list and the borrow of the scope tree
/// cannot be confused for each other while the walk is filling one from the other.
fn fact_for(
    root: &CppSyntaxNode,
    shapes: &DeclarationShapes,
    binding: &Binding,
    scope: Option<String>,
    local: bool,
    // **The namespaces the enclosing scope sits in** — see [`enclosing_namespaces_of`]. Passed in rather than asked
    // here because **every binding of one scope has the same answer**, and asking per binding was a scan of every
    // scope in the file per declaration.
    in_namespace: Option<String>,
    declarations: &Declarations<'_>,
    // than the macro table, because the answer is the same for every declaration in the file and asking per
) -> Option<DeclFact> {
    // A binding whose name has no identifier is a destructor, an operator, or a conversion function. Its
    // **spelling** is still a name a lookup can be keyed on — `std::vector::~vector`, `std::vector::operator=` —
    // and `Name::text` is what produces it, so that is what is stored.
    //
    // It used to be stored **empty** ("a declaration is here" and nothing more), and the measurement that changed
    // it is why: `DeclFact::qualified_name` of a nameless fact *is* its scope, so every destructor answered for
    // its own class. `definition("std::vector")` then found the class **and** its destructor and reported
    // `Ambiguous`, which is how MSVC's STL answered "not declared" for every member query (
    // §4.2 ①). The declaration stays findable by position, which was the point of keeping it at all.
    let name = binding.name.text();
    // **A qualified declarator name declares into the scope it names.**
    //
    // `void Name::printf(int i) { }` is a declaration **of `Name`'s `printf`**, not a file-scope name spelled
    // `Name::printf`, and the difference is every lookup that matters: a member access, a hover on the declaration,
    // `definitions("printf")` and `definitions_in_scope("Name::printf")` all read `name` and `scope`.
    //
    // The scope walker keeps such a name qualified on purpose (see `sema::scopes`: a binding of `count` in the
    // enclosing scope would be a name that cannot be used unqualified there), so the split belongs here, where the
    // fact is built. Until the grammar learned the shape, an out-of-class definition reached this point with its name
    // swallowed by the specifier sequence; now the declarator's name *is* the qualified spelling.
    let (name, scope) = match name.rsplit_once("::") {
        // A local never carries a qualifier: `S::f` at block scope is a call, not a declaration.
        Some((qualifier, last)) if !local && !qualifier.is_empty() && !last.is_empty() => (
            last.to_string(),
            Some(qualifier.trim_start_matches("::").to_string()),
        ),
        _ => (name, scope),
    };

    // **The four type questions, timed apart.** Together they are 98% of the index's sweep on a real project
    // (138 files of MSVC's standard library), and each one is a different traversal of the same tree — so which of
    // them dominates is the difference between four different fixes. See `crate::stages`.
    // **A specifier sequence interrupted by a directive states two types, not one.**
    //
    // `bits/stl_bvector.h` writes `emplace_back`'s return type as
    //
    // ```text
    // #if __cplusplus > 201402L
    //     _GLIBCXX20_CONSTEXPR
    //     reference
    // #else
    //     void
    // #endif
    //     emplace_back(_Args&&... __args)
    // ```
    //
    // and the specifier sequence the type is read from is literally that text: `declared_returns_of_with` walks it,
    // steps over the directives, and answers with the nearest type — the `#else` branch's `void`, in a C++20 project
    // where the function returns `reference`. Measured over three standard-library headers (`facts_vs_rendering`):
    // **12 of 1589** facts disagree with the rendering about the return type, and every one is this shape.
    //
    // The written reading cannot know which branch is live — that is the preprocessor's answer, and it needs the
    // compiler configuration a summary is deliberately not keyed by. So the honest fact says nothing rather than a
    // branch: a consumer holding a **rendering** (the reading where the directive is resolved) has the real type, and
    // one that holds only this summary is told the truth.
    let split_by_a_directive = split_by_a_directive_with(shapes, binding);
    let type_of = if split_by_a_directive {
        None
    } else {
        declared_type_of_with(shapes, binding)
            .or_else(|| declared_alias_target_with(shapes, binding))
            // **A namespace alias, recognised by its shape.** See [`declared_namespace_alias_target_with`] for why the
            // character after the name cannot answer: the binding's range and the text `fact_for` is handed are not
            // always the same ruler, and a header's alias was filed with no target because of it.
            .or_else(|| declared_namespace_alias_target_with(root, binding))
    };
    let returns = {
        // **The written return type, and the deduced one when there is none to write.** `declared_returns_of_with`
        // answers `None` for `auto`, by design and for a reason that has not changed — `auto` is not a class
        // anything can be looked up in. What is new is that the **body** is in this function's hands, and a `return`
        // statement states a type where a specifier sequence does not. See [`deduced_returns_of`] for how little of
        // that is provable at this layer, and why the rest is refused rather than approximated.
        //
        // **An operator's written return type is kept only when a fact may carry it** — see
        // [`an_operator_return_is_usable`]; this is the index's own writer, so the gate is asked here as well as in
        // [`declared_returns_of_in`].
        if split_by_a_directive {
            None
        } else {
            declared_returns_of_with(shapes, binding)
                .filter(|spelling| {
                    binding.kind != BindingKind::OperatorFunction
                        || an_operator_return_is_usable(root, binding, spelling)
                })
                .or_else(|| deduced_returns_of(root, binding))
        }
    };
    let bases = { declared_bases_of_with(shapes, binding) };
    // The class template's parameter names, for the members whose types are written with them. Answered from the
    // shapes like the four questions above it, and on a unit read it is the reason they are all asked of a table
    // rather than of the tree — see [`declared_template_parameters_of`].
    let parameters = { declared_template_parameters_with(shapes, binding) };
    // **The constraint in front of each of those names**, from the same head — see [`DeclFact::constraints`]. A
    // check at another file`s call site can only ask the summary, and names without their constraints cannot say
    // whether an argument satisfied what the declaration asked for.
    let constraints = { declared_constraints_with(shapes, binding) };
    // **And what each of them defaults to**, which is the other half of a use that writes fewer arguments than the
    // declaration has parameters — see [`DeclFact::parameter_defaults`].
    let parameter_defaults = { declared_parameter_defaults_with(shapes, binding) };
    // **Who may name it.** Recorded on the shape the binding's name sits in, which the pass that built this table
    // already knows — see [`Shape::access`]. `None` for a name no class body contains.
    let access = shapes.access_at(the_offset_to_descend_by(binding));
    // **…and whether an importer may.** The other half of "who may name it", for a file that declares a module:
    // see [`Shape::exported`] and [`crate::ProjectIndex::visible_files`], which is where it is applied.
    let exported = shapes.exported_at(the_offset_to_descend_by(binding));
    // **What the function was declared with**, which is the one thing a reader picking a name out of a hundred
    // needs and a fact did not carry: `format` and `format_to` are two rows of a completion that used to read
    // `string (…)` and `_OutputIt (…)`. Read out of the shape table, which recorded the declarator in the pass that
    // already walked the file — see [`declared_parameters_with`] for what that replaced.
    let parameter_list = match binding.kind {
        BindingKind::Function
        | BindingKind::Constructor
        | BindingKind::Destructor
        | BindingKind::ConversionFunction
        | BindingKind::OperatorFunction
        | BindingKind::LiteralOperator => declared_parameters_with(shapes, binding),
        _ => None,
    };
    // **A name no declarator holds** — a constructor written with a specifier (`explicit A(int x)`) keeps its name in
    // the specifier sequence, so `names_the_binding` is false for its declarator and the list was lost: measured, the
    // fact came out as `A` with an empty `parameter_list`, which a completion row and a signature help both read as a
    // constructor that takes nothing. The declaration is found from the binding's own range, and its `ParameterList` is
    // the one written there.
    let parameter_list = parameter_list.or_else(|| {
        if !matches!(
            binding.kind,
            BindingKind::Constructor | BindingKind::Destructor | BindingKind::ConversionFunction
        ) {
            return None;
        }
        use cpp_parser::CppAstNode;
        let token = cpp_parser::token_at(root, binding.range.start_offset)?;
        let parent = token.parent()?;
        let declaration = std::iter::once(parent.clone())
            .chain(parent.ancestors())
            .find(|node| cpp_parser::CppDeclaration::can_cast(node.kind().into()))?;
        let declaration = cpp_parser::CppDeclaration::cast(declaration)?;
        let list = declaration.get_param_list()?;
        Some(list.syntax().text().to_string())
    });

    // **Where a local sits, for the names it writes.** A declaration inside a body has no qualified name of
    // its own — see [`DeclFact::local`] — so the namespaces around it are the only place a type it writes can
    // be looked up from. Asked once per **scope** rather than once per declaration, and the scope tree is where the
    // closure's macro bodies were in hand when it was built: a view's own scope tree has no macro evidence and
    // cannot see a namespace a macro opened ([`crate::FileView::parse`]), which is exactly the case every
    // standard-library local is.
    let in_namespace = if local { in_namespace } else { None };

    // **The class's name as the declaration wrote it, when that name carries a pattern.** See [`DeclFact::pattern`]
    // for the wrong answer this exists to remove — a partial specialization's member answering for an instantiation
    // that does not match it.
    let pattern = pattern_of(shapes, binding);

    // **Is the scope this declaration reports one the file's text could actually see?**
    //
    // `scope: None` means "declared at file scope", and it is that answer because the text says so. A namespace
    // opened by a **macro** leaves no trace in a reading that does not expand macros — the `{` is inside the macro's
    // body — and MSVC's headers are written that way from the first line (`_STD_BEGIN` is `namespace std {`). So the
    // declarations inside it were filed as if they were at file scope, and the qualified name they really have was
    // unrecoverable: measured, `std::endl` answered `NotDeclaredHere` while `__msvc_ostream.hpp` held `endl` with
    // `scope: None`, and the popup was empty while `std::vector::size` — filed with a scope the cooked reading could
    // see — worked.
    //
    // The evidence is **an unexpanded macro invocation at file level**: a token that this compilation defines as a
    // macro and that the reading left standing as an identifier. That is not a guess about what the macro expands
    // to — it is the reading saying "there is something here I could not process", which is exactly what makes the
    // scope it reports untrustworthy. A file whose macros are all inside conditions, or inside bodies, invokes none
    // at file level and keeps the file scope it earned.

    Some(DeclFact {
        kind: DeclKind::from_binding_kind(binding.kind),
        // **What the declaration writes** — read from the same shape table as everything above, and stored so that a
        // check asking "does this say `override`" does not search the text for it. See [`crate::DeclModifiers`].
        modifiers: declared_modifiers_with(shapes, binding),
        // **The `requires`-clause written after the parameter list**, which is a fact about the declaration and not a
        // modifier: `void f(int) const requires (x > 0)`. The hover card showed neither it nor the `const` — reported
        // from `strings.cpp` as "the card is missing what the declaration says".
        requires_clause: the_requires_clause_with(shapes, binding),
        // **What the function frees** is filled by the pass after the facts are built — see
        // [`fill_the_frees`], which needs the whole file to do it in one walk.
        frees: 0,
        // **The annotation is filled after the facts exist** — see `fill_the_reflection`, which reads the file text once
        // for the whole file rather than once per declaration.
        reflection: None,
        name,
        scope,
        // Asked of the scope the binding was made in rather than of the declaration's shape: `bool` is the one
        // answer a *shape* cannot give, because `void f() { int x; }` and `void f() { }` differ by a declaration
        // that is not in a scope at all. See [`ScopeTree::declares_a_local`].
        local,
        in_namespace,
        type_of,
        returns,
        split_by_a_directive,
        bases,
        parameters,
        constraints,
        parameter_defaults,
        pattern,
        parameter_list,
        access,
        exported,
        range: binding.range,
        name_range: binding.name_range,
        // The one field that comes from the diagnostics rather than from the tree — see [`DeclFact::clean`] for
        // what the answer does and does not claim.
        clean: declarations.is_clean(binding.name_range, binding.range),
        // **A definition, or a declaration that only names the thing** — the difference between `struct _Ty;` and
        // `struct _Ty { … };`, which MSVC's standard library writes both of. Read from the node: a class body, an
        // enum body, a function body.
        definition: declarations.declares_a_body(binding.name_range),
        // Filled in by `assign_guards`, which is the only place that knows where the directives are.
        guard: FactGuard::Unconditional,
    })
}

/// **The default argument of each of those parameters**, in the declaration's order — see
/// [`DeclFact::parameter_defaults`].
///
/// Empty for the overwhelming majority of templates (the field is a row of `""` and is not stored at all), and empty
/// for anything that is not a template, which is the same guard [`declared_template_parameters_with`] applies.
pub(crate) fn declared_parameter_defaults_with(
    shapes: &DeclarationShapes,
    binding: &Binding,
) -> Vec<String> {
    if declared_template_parameters_with(shapes, binding).is_empty() {
        return Vec::new();
    }

    let at = binding.name_range.start_offset;
    let Some((_, head)) = shapes
        .templates
        .iter()
        .rev()
        .find(|(introduced, _)| introduced.start_offset <= at && at <= introduced.end_offset())
    else {
        return Vec::new();
    };

    let defaults = crate::sema::types::template_parameter_defaults(head);
    // **A row of "no default" is not stored.** The field is cloned and compared per fact, and most templates have no
    // default at all: storing the empties would put sixteen bytes on every class template in the standard library to
    // say nothing.
    if defaults.iter().any(|default| !default.is_empty()) {
        defaults
    } else {
        Default::default()
    }
}

/// **A class declaration's own text, up to its body** — read child by child instead of from the node.
///
/// # What this replaces, and what it cost
///
/// It used to be `declaration.text().to_string()`: the whole class, body included. `pattern_of` asks for it once per
/// **member**, so a class of M members and size S cost M × S. MSVC's `basic_string` has a couple of hundred members
/// and a body of about a hundred kilobytes, which is tens of megabytes of string building **for one class** — and
/// `xstring` has many classes.
///
/// That was the whole of the index's cost on a real project, and it was invisible because the timer covering it
/// (`Stage::Facts`) covers five other questions too, and those five are the ones with names:
///
/// ```text
///   indexes 126 files of MSVC's standard library closure, CPU time
///     parse                          996.7 ms   11.2%
///     sweep                         7293.1 ms   82.3%   ← the reading below is what was in it
///       facts                       6845.0 ms
///         type-of                      104.4            the five questions this function is *named* for
///         returns                      117.0
///         bases                          2.4
///         template-params                5.6
///         alias                          0.5
///         …the rest, ~6.6 s              pattern_of, per member, materialising its class
/// ```
///
/// # Why the head and not the whole text
///
/// The caller reads the head: the keyword that introduces the class, the name, and the template argument list —
/// everything before the first `{` or `;`, which is exactly what the cut below keeps. So walking the children and
/// stopping at the body is not an optimisation of a different answer; it is the same answer without the part the
/// caller throws away.
///
/// A child that **starts** with a brace is the body, and its text is the one thing this function exists not to
/// build. Every other child of a class declaration is a keyword, a name, a base clause or an attribute.
fn class_head_of(declaration: &CppSyntaxNode) -> String {
    let mut head = String::new();
    for child in declaration.children_with_tokens() {
        if let Some(token) = child.as_token() {
            head.push_str(token.text());
        } else if let Some(node) = child.as_node() {
            if node.first_token().is_some_and(|token| token.text() == "{") {
                break;
            }
            head.push_str(&node.text().to_string());
        }
        if head.contains(['{', ';']) {
            break;
        }
    }
    head
}

/// **The pattern the innermost class around this declaration was written with** — see [`Shape::pattern`].
///
/// Answered from the shape table, which recorded it once per class, instead of by descending from the root to the
/// declaration. The two are the same question: the shapes an offset is inside are exactly the declarations it is
/// inside, so the innermost class-like shape on that path **is** `enclosing_class_of`'s answer, and the pattern
/// stored on it is what this returns — including when it is `None`, because a plain `struct Widget` is the answer
/// for its members and the class *further out* is not.
pub(crate) fn pattern_of(shapes: &DeclarationShapes, binding: &Binding) -> Option<String> {
    let mut found = None;
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        if matches!(
            shape.kind,
            CppSyntaxKind::ClassDef | CppSyntaxKind::StructDef | CppSyntaxKind::UnionDef
        ) {
            found = shape.pattern.clone();
        }
    });
    found
}

/// **A class-like definition's written name, up to its body, when it carries a template argument list.**
///
/// **One local declaration, and every identifier in the same file that resolves to it.**
///
/// # Why this is a fact about one file and nothing else
///
/// A local declaration cannot be named from another file — not by an `#include`, not by a qualified
/// name, not by any path through the include graph. So every use of one is in the file that
/// declares it, and a table keyed by the declaration's own offset is a **single-file fact**: it
/// needs no closure, no macro environment and no timeline.
///
/// # Why the key is an offset and not a name
///
/// Two locals in one file share a spelling all the time — `for (auto& item : a)` beside `for (auto& item : b)` —
/// and they are different declarations. `declared_at` is the declaration's own name, which is unique in a file, so
/// the table cannot merge them; the spelling is recoverable from the declaration when a consumer needs it, and the
/// identity is not recoverable from the spelling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocalReferences {
    /// The declaration's own name, as an offset into the file — the key. `DeclFact::name_range.start_offset` of the
    /// local declaration, so a consumer holding a fact finds its uses by one lookup.
    pub declared_at: u32,
    /// The offsets of every identifier that resolves to it, in document order.
    pub uses: Vec<u32>,
}

/// **Resolve every identifier in a file to the local declaration it names**, when it names one.
///
/// # What "resolves" means, and what it deliberately does not
///
/// The rule is C++'s own lookup for a name written inside a function: from the innermost scope containing the
/// identifier outward, the first scope declaring the name wins, and an outer declaration of the same spelling is
/// **shadowed** rather than also counted. That is a resolution and not a spelling match — the difference between
/// this and `ProjectIndex::symbol_references`'s identifier scan, where every same-spelled token in every candidate
/// file is reported as a use.
///
/// It is deliberately not a full resolver, and the limits are the ones this crate states everywhere else:
///
/// * **locals only.** A name resolving to a class member, a namespace member or a template parameter is not in the
///   table — those are the cross-file index's business and answering them needs the closure, which would make this
///   a fact about the closure rather than about the file.
/// * **no overload selection** (there is no overload resolution in this crate yet), so two declarations of one name
///   in one scope resolve to the first.
/// * a use inside a **lambda** resolves against the lambda's own scope, so a capture counts as a use of the
///   enclosing local. That is the wanted answer for a rename, and it is stated rather than discovered.
///
/// # Cost
///
/// One descent per identifier plus one walk of the scope chain per identifier that has a binding, over a tree the
/// parser has already produced — no closure walk, no macro environment, no text read. The descent is `O(siblings)`
/// per level, the shape [`enclosing_class_of`] had before it became a table lookup, and it is paid per
/// **identifier**; `examples/use_index_cost.rs` is where that volume is read.
pub fn local_references_of(scopes: &ScopeTree, root: &CppSyntaxNode) -> Vec<LocalReferences> {
    // The scope the file itself is: the only one with no parent, and the root of the descent below.
    let Some(file_scope) = scopes
        .scopes()
        .iter()
        .position(|scope| scope.parent.is_none())
        .map(ScopeId)
    else {
        return Vec::new();
    };

    // A declaration's own name is not a use of it.
    let mut declared: HashSet<usize> = HashSet::new();
    for scope in scopes.scopes() {
        for binding in &scope.bindings {
            declared.insert(binding.name_range.start_offset);
        }
    }

    let mut found: HashMap<u32, Vec<u32>> = HashMap::new();

    // **A name index per scope, built once.** `Scope::bindings` is a `Vec` on purpose — a name may be declared twice
    // — and that makes the lookup this pass needs a linear scan. Measured the other way: scanning it once per
    // identifier cost **9.2 s over 126 files, 30.8 µs per identifier**, because the file scope's own binding list
    // holds every top-level declaration in the file and every identifier walked it. The map is the same information
    // arranged for the question being asked 299 143 times.
    let mut by_name: HashMap<usize, HashMap<String, &Binding>> = HashMap::new();
    for (index, scope) in scopes.scopes().iter().enumerate() {
        let mut names: HashMap<String, &Binding> = HashMap::new();
        for binding in &scope.bindings {
            // First wins, which is the order the linear scan had. One `String` per **binding** — `Name::text`
            // allocates, and that is affordable here and not per identifier.
            names.entry(binding.name.text()).or_insert(binding);
        }
        by_name.insert(index, names);
    }

    for element in root.descendants_with_tokens() {
        let Some(token) = element.as_token() else {
            continue;
        };
        if cpp_parser::CppTokenKind::from(token.kind()) != cpp_parser::CppTokenKind::Identifier {
            continue;
        }
        let at = usize::from(token.text_range().start());
        if declared.contains(&at) {
            continue;
        }
        let name = token.text();
        if name.is_empty() {
            continue;
        }

        let Some(binding) = binding_for(scopes, &by_name, file_scope, at, name) else {
            continue;
        };
        if !matches!(
            scopes
                .scope(binding.scope)
                .map(|scope| scope.kind)
                .unwrap_or(ScopeKind::TranslationUnit),
            ScopeKind::Function | ScopeKind::Block | ScopeKind::Lambda
        ) {
            continue;
        }

        found
            .entry(binding.name_range.start_offset as u32)
            .or_default()
            .push(at as u32);
    }

    let mut references: Vec<LocalReferences> = found
        .into_iter()
        .map(|(declared_at, uses)| LocalReferences { declared_at, uses })
        .collect();
    references.sort_by_key(|entry| entry.declared_at);
    references
}

/// **Which declaration the name written at `at` refers to**, by C++'s own rule: the innermost scope declaring it.
///
/// The descent to the innermost scope containing `at` and the walk back out along the chain, in one function,
/// because they are one question — a caller holding the scope would not need this, and a caller with only an offset
/// cannot ask anything cheaper.
fn binding_for<'a>(
    scopes: &'a ScopeTree,
    by_name: &HashMap<usize, HashMap<String, &'a Binding>>,
    file_scope: ScopeId,
    at: usize,
    name: &str,
) -> Option<&'a Binding> {
    let mut current = file_scope;
    // Down to the innermost scope whose range holds the offset. Scopes' children partition their parent's range the
    // way syntax children do, so at most one child contains `at` and the loop is the depth of the scope tree.
    while let Some(child) = scopes.scope(current).and_then(|scope| {
        scope.children.iter().copied().find(|child| {
            scopes
                .scope(*child)
                .and_then(|scope| scope.range)
                .is_some_and(|range| at >= range.start_offset && at < range.end_offset())
        })
    }) {
        current = child;
    }

    // …and back out: the first scope on the chain that declares the name is the one the language binds it to.
    let mut scope = Some(current);
    while let Some(id) = scope {
        let data = scopes.scope(id)?;
        if let Some(binding) = by_name.get(&id.index()).and_then(|names| names.get(name)) {
            return Some(binding);
        }
        scope = data.parent;
    }

    None
}

/// `Some("atomic<_Ty>")` for `template <class _Ty> struct atomic<_Ty> { … }`, `None` for `struct Widget { … }` —
/// the `<` is the test, and it is the whole of what the field means: a partial specialization's members must not
/// answer for an instantiation that does not match them.
///
/// `None` also for anything that is not a class-like definition, which the caller has already established.
fn written_class_pattern(declaration: &CppSyntaxNode) -> Option<String> {
    let text = class_head_of(declaration);

    // **The head only.** The declaration's text is the whole class — body included — and a body holds `class` and
    // `struct` keywords of its own (nested classes, `using` declarations). A walk that took the last one in the text
    // pointed into the body, produced a spelling with no `<` in it, and the field was `None` for every class in the
    // standard library.
    //
    // A forward declaration has no `{`, and ends at its `;` — cut there too, so `struct Widget;` is the same
    // nothing-to-see as a body.
    let head = text
        .find(['{', ';'])
        .map(|end| &text[..end])
        .unwrap_or(&text[..]);

    // The keyword that introduces it, and the last one before the name: `template <class _Ty> struct atomic<…>` has
    // `class` inside the parameter list and `struct` before the name, so the search runs over the **head** and the
    // last match is the one that introduces the class.
    let mut at = 0usize;
    let mut found = None;
    for (index, _) in head.match_indices(|character: char| character.is_alphabetic()) {
        if index < at {
            continue;
        }
        let word = &head[index..];
        let end = word
            .find(|character: char| !character.is_alphanumeric() && character != '_')
            .unwrap_or(word.len());
        let word = &word[..end];
        if matches!(word, "class" | "struct" | "union") {
            found = Some(index + end);
        }
        at = index + end.max(1);
    }

    let after = &head[found?..];
    let end = after.find([':', '{', ';']).unwrap_or(after.len());
    let written = after[..end].trim();

    written.contains('<').then(|| written.to_string())
}

/// **The innermost class-like declaration containing this binding**, when there is one.
///
/// # Why this used to descend, and why it no longer does
///
/// The first version took `root.descendants()`, kept the class-like nodes whose range contains the offset, and took
/// the smallest. It is correct and it is **quadratic**: `pattern_of` asked this once per declaration, so a file cost
/// declarations × nodes. Measured on eight MSVC headers, the same probe went from about thirty seconds to **over ten
/// minutes**, and a real workspace pays it on every file it indexes at startup — which is what made loading a project
/// crawl.
///
/// The second version walked **down** from the root and kept only the children containing the offset. That fixed the
/// quadratic term and was still, measured later, **2245 ms of a 4562 ms `Facts` stage** over 126 files: the descent
/// costs `O(siblings)` at *every* level, it ran once per declaration, and the overwhelming majority of declarations
/// are not in a class at all — so most of the work was spent discovering that.
///
/// It is now a **table lookup**: [`DeclarationShapes`] already records every declaration-like node and the chain it
/// sits in, in one pass over the file, and [`Shape::pattern`] carries the class's own spelling because that is what
/// the one caller wanted. What is left of this question is [`pattern_of`], five lines of it.
///
/// Kept as a note rather than as code: the function itself is gone, and the two measurements above are the reason
/// nobody should reintroduce a per-declaration descent here.
///
/// **The namespaces enclosing `at`, outermost first** — or `None` when only the file does.
///
/// The answer a local declaration needs and [`ScopeTree::qualification_prefix_of`] cannot give: that function
/// answers `None` for a function body, a block and a lambda on purpose — a local is not named *through* them —
/// and `None` is the right answer to "what is this declaration qualified by" while being no answer at all to
/// "what can a type written here be looked up from". The second question skips the bodies and keeps the
/// **namespaces**, which is what C++ does when it looks a name up from inside a function.
///
/// Measured: `void f() { _Basic_format_specs s; }` inside `std` is a type no lookup could place, and the
/// namespace that would have placed it is two scopes out and spelled by no token in the file.
/// **The namespaces the scope `from` sits in, outermost first** — or `None` when only the file encloses it.
///
/// The answer a local declaration needs and [`ScopeTree::qualification_prefix_of`] cannot give: that function
/// answers for the scope *itself*, and a local's namespaces are those of the scopes **around** it.
///
/// # What this used to be, and what it cost
///
/// It took an **offset** and asked [`ScopeTree::scope_at`] which scope contained it — and `scope_at` is a scan of
/// every scope in the file:
///
/// ```text
///   self.scopes.iter().enumerate().filter(… contains the offset …).min_by_key(… smallest range …)
/// ```
///
/// It ran once per **local declaration**, so a file cost declarations × scopes, and MSVC's headers are mostly
/// inline function bodies — both factors large. The caller already knows the scope: [`DeclarationFacts::build`] is
/// iterating that scope's own bindings when it asks.
///
/// # Why the answer is the same one
///
/// `scope_at(offset)` is the **innermost** scope containing the binding's name; the scope being iterated is an
/// ancestor-or-equal of it. The two can only disagree by the scopes strictly between them, and the question filters
/// for `ScopeKind::Namespace` — which cannot be opened inside a class or a function, so no namespace can sit
/// strictly between a binding's scope and a scope nested inside it. A `union`'s body, a lambda's, a `for`'s: none of
/// them is a namespace, and the chain outward from either starting point meets the same ones.
fn enclosing_namespaces_of(scopes: &ScopeTree, from: ScopeId) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for id in scopes.scope_chain(from) {
        let Some(data) = scopes.scope(id) else {
            continue;
        };
        if data.kind == ScopeKind::Namespace
            && let Some(name) = data.name.as_deref()
        {
            segments.push(name);
        }
    }

    if segments.is_empty() {
        return None;
    }

    segments.reverse();
    // **A namespace that opens inside one of the same name is that namespace reopened.**
    //
    // The chain is read from the scope tree, and in a *unit rendering* — the stitched stream a module's interface unit
    // produces — one file's `namespace std {` can land inside another's. MSVC's headers write every scope as
    // `_STD_BEGIN`, so the standard module came out with ten of them nested and `std::format` filed under
    // `std::std::std::std::std::std::std::std::std::std` — a scope no query asks for, while the same header read on its
    // own is filed under `std`. Collapsing a run of the same name makes the two spellings agree.
    //
    // What it costs: `namespace A { namespace A { void f(); } }` is `A::A::f` to a compiler, and this answers `A::f`.
    // That is a real name a query could ask for differently — and it is also the only reading under which a `std`
    // reopened by ten spliced headers keeps the name the source files give it. The parser is where the nesting should
    // have been stopped (`tests/translation_unit.rs`'s `a_namespace_whose_brace_ends_the_file` is the shape); this is
    // the layer that has to answer until it is.
    segments.dedup();
    Some(segments.join("::"))
}

/// One declaration node of the file, and whether a diagnostic fell inside it.
struct Declared {
    range: cpp_parser::SourceRange,
    touched: bool,
    /// **Does this declaration carry the body that defines it** — a class body, a function body? Read in the same one
    /// pass, because the question is asked once per fact and a per-fact scan of the tree was measured at minutes.
    has_body: bool,
}

/// The declaration nodes of one file, in the form [`DeclFact::clean`]'s question needs them.
///
/// # The three rules that were measured
///
/// Over the closure of `<vector>` (279 files, 8499 declarations, 5388 of them in files that do not parse
/// cleanly), asked of every declaration in a file with diagnostics:
///
/// | what is asked about | marked unclean |
/// |---|---|
/// | the fact's own range, which is the declarator | 106 |
/// | the innermost declaration node containing the name | 214 |
/// | **any** declaration node containing the name | 2907 |
/// | the file | 5388 |
///
/// The third row is the one that looks simplest and is not affordable: a class or namespace body with one error
/// in it condemns every member, which is more than half of the standard library's declarations. The second row is
/// 108 more than the first, and those 108 are the point of the exercise — measured by kind, 89 are `Declaration`
/// nodes and 19 are `TemplateDecl` nodes, which is exactly the **type part** and the **template header**: text
/// outside the declarator and inside the declaration, and the text a fact's `type_of` is read from.
///
/// # Why the node set is the walker's own
///
/// The kinds are [`crate::scopes::declaring_kinds`], the set this layer already reads declarations out of, so
/// "was the declaration recovered from" cannot drift from "what counts as a declaration". That coupling is a
/// shape coupling between the symbol layer and this one and is deliberate; a construct added to the grammar and
/// to that list is a declaration here too, and one added to neither is invisible to both.
struct Declarations<'a> {
    /// Shortest first, which is what makes the scan below find the **innermost** declaration: the nodes are
    /// nested, so the shortest one containing a name is the one that name was written in.
    nodes: Vec<Declared>,
    /// The diagnostics, for the one case the nodes cannot answer.
    errors: &'a [SourceRange],
}

impl<'a> Declarations<'a> {
    /// Read every declaration node of the file, and mark the ones a diagnostic falls inside.
    fn of(root: &CppSyntaxNode, errors: &'a [SourceRange]) -> Self {
        let mut nodes = Vec::new();

        // **The bodies, in one pass** — see [`Declared::has_body`]. Collected before the declarations so that each
        // declaration costs a binary search rather than a walk of its own subtree: the per-fact version of this
        // question is O(facts × nodes), which on a 1.99 MB unit rendering is minutes of CPU.
        let mut bodies: Vec<(usize, usize)> = Vec::new();
        for node in root.descendants() {
            let kind = CppSyntaxKind::from(node.kind());
            if matches!(kind, CppSyntaxKind::ClassBody | CppSyntaxKind::CompoundStat) {
                let range = cpp_parser::source_range(node.text_range());
                bodies.push((range.start_offset, range.end_offset()));
            }
        }
        bodies.sort_unstable();

        // **Built always**, not only for a file with diagnostics: the body question is asked of every declaration in
        // every file, and the `touched` answer is the only part that needs the diagnostics (`is_clean`'s rule is
        // unchanged — a short list is what it reads).
        {
            for node in root.descendants() {
                let kind = CppSyntaxKind::from(node.kind());
                if !crate::scopes::declaring_kinds().contains(&kind) {
                    continue;
                }

                let range = cpp_parser::source_range(node.text_range());
                // A body written **inside** this declaration, not one that merely overlaps it.
                let start = bodies.partition_point(|(body, _)| *body < range.start_offset);
                let has_body = bodies[start..]
                    .iter()
                    .take_while(|(body, _)| *body < range.end_offset())
                    .any(|(body, end)| *body >= range.start_offset && *end <= range.end_offset());
                nodes.push(Declared {
                    range,
                    touched: errors.iter().any(|error| {
                        error.start_offset < range.end_offset()
                            && range.start_offset < error.end_offset()
                    }),
                    has_body,
                });
            }

            nodes.sort_by_key(|declared| declared.range.length);
        }

        Declarations { nodes, errors }
    }

    /// **Does the declaration this name was written in carry a body** — a class body, a function body?
    ///
    /// The innermost declaration node containing the name, the same rule [`Declarations::is_clean`] uses. The answer is
    /// what tells `struct _Ty;` from `struct _Ty { … };`, and it is read from the one pass [`Declarations::of`] makes
    /// rather than from a walk of the tree per fact.
    fn declares_a_body(&self, name_range: SourceRange) -> bool {
        match self
            .nodes
            .iter()
            .find(|declared| contains(declared.range, name_range))
        {
            Some(declared) => declared.has_body,
            None => false,
        }
    }

    /// Was the declaration this fact was written in free of diagnostics?
    ///
    /// The innermost declaration node containing the **name** is what is asked about, rather than the fact's own
    /// range: the fact's range is the declarator, and a diagnostic in the type of what it declares is outside it
    /// and inside the declaration — the measurement above counts 108 of those.
    ///
    /// The fallback is for a fact whose name no declaration node contains, which is a binding the walk found
    /// outside the constructs it reads declarations from: there the fact's own range is all there is to ask
    /// about. A file with no diagnostics has an empty node list and answers `true` for every fact, which is the
    /// truth and is what the tests that do not care about this field rely on.
    fn is_clean(&self, name_range: SourceRange, own_range: SourceRange) -> bool {
        match self
            .nodes
            .iter()
            .find(|declared| contains(declared.range, name_range))
        {
            Some(declared) => !declared.touched,
            None => !self.errors.iter().any(|error| overlaps(*error, own_range)),
        }
    }
}

/// Is `inner` inside `outer`?
fn contains(outer: SourceRange, inner: SourceRange) -> bool {
    outer.start_offset <= inner.start_offset && inner.end_offset() <= outer.end_offset()
}

/// Do the two ranges share a token position?
fn overlaps(one: SourceRange, other: SourceRange) -> bool {
    one.start_offset < other.end_offset() && other.start_offset < one.end_offset()
}

/// The walk's state: the regions opened so far, and the facts collected.
struct DeclarationFacts<'a> {
    scopes: &'a ScopeTree,
    preprocessing: &'a FilePreprocessing,
    /// The tree the facts are read from, for the two questions that are not on the path to a name: a class
    /// template's parameter list is a *sibling* of the class body, and a function's parameters are inside a
    /// declarator rather than on the path to the name — see [`declared_template_parameters_of`] and
    /// [`parameter_list_at`].
    root: &'a CppSyntaxNode,
    /// **The file's declarations, read once** — where a declared type's spelling comes from. Built here rather than
    /// per binding: that difference is 14.4 s against a lookup, see [`DeclarationShapes`].
    shapes: DeclarationShapes,
    /// The declarations the diagnostics fall inside, computed once — see [`Declarations`].
    declarations: Declarations<'a>,
    guards: SummaryGuards,
    /// Every fact, in the order the scopes hold them — sorted by offset once, before the guard sweep.
    facts: Vec<DeclFact>,
}

impl<'a> DeclarationFacts<'a> {
    fn new(
        scopes: &'a ScopeTree,
        preprocessing: &'a FilePreprocessing,
        root: &'a CppSyntaxNode,
        errors: &'a [SourceRange],
    ) -> Self {
        DeclarationFacts {
            scopes,
            preprocessing,
            root,
            shapes: { DeclarationShapes::of(root) },
            declarations: Declarations::of(root, errors),
            guards: SummaryGuards::default(),
            facts: Vec::new(),
        }
    }

    fn build(self) -> (Vec<DeclFact>, SummaryGuards) {
        // Destructured so that the walk below holds the inputs and the two outputs as separate bindings: the loop
        // pushes into `facts` while reading `shapes` and `declarations`, which a `&mut self` method could not do.
        let DeclarationFacts {
            scopes,
            preprocessing,
            root,
            shapes,
            declarations,
            mut guards,
            mut facts,
        } = self;

        // per declaration copies the whole file each time.

        // **The rendered file, once.** `root` is the tree of the *rendering* — macros expanded, directives resolved —
        // and the only modifier that needs text rather than tokens is an attribute written before a declaration, which
        // the parser keeps as a sibling. One copy per file, not one per declaration.
        let rendered = root.text().to_string();

        for (index, scope) in scopes.scopes().iter().enumerate() {
            let id = ScopeId(index);
            // **A template parameter list files no declarations, and filing them was expensive in a way that had
            // nothing to do with templates.** A parameter is visible only inside the declaration it parameterizes,
            // which the index has no way to say: `qualification_prefix_of` skips this scope kind, so every
            // `template <class _Tp>` in every header was stored as a `Type` declaration **at file scope**, named
            // `_Tp`. Measured on a real project: **2288** facts named `_Tp` (492 of them in `<type_traits>` alone),
            // and one lookup of that name — which the engine asks whenever a declaration's type is a template
            // parameter it could not substitute — took **167–187 ms** and answered 2288 declarations, none of which
            // is a class. That one lookup was 177 of the 199 ms `implicit-bool-conversion` gained the moment operator
            // return types began resolving; it was never the operator's cost.
            //
            // The names are not lost: a class template's and a function template's parameters are on the **fact**
            // (`DeclFact::parameters`), which is where the substitution step reads them.
            if scope.kind == ScopeKind::TemplateParameters {
                continue;
            }
            // **A body answers both questions by itself**, and it is asked here rather than inside the two functions
            // because most scopes in a header are bodies. `qualification_prefix_of`'s own match lists exactly these
            // three kinds as `None` — a declaration written in a function body, a block or a lambda is qualified by
            // nothing — and `declares_a_local` walks the chain and finds the scope's own kind, so a body is always
            // local. Both walks were being paid for every `{` in every inline function of every header, and the
            // answers could not have been anything else.
            //
            // This is the cheap half of what clangd gets by not parsing header bodies at all
            // (`SkipFunctionBodies` during the preamble): the bodies are still parsed here, but the *index* stops
            // asking questions about them whose answer is fixed.
            let (prefix, local) = match scope.kind {
                ScopeKind::Function | ScopeKind::Block | ScopeKind::Lambda => (None, true),
                _ => (
                    scopes.qualification_prefix_of(id),
                    scopes.declares_a_local(id),
                ),
            };

            // **And the namespaces, once per scope rather than once per binding.** Every binding of one scope sits
            // in the same namespaces, and asking per binding was a scan of every scope in the file each time — see
            // [`enclosing_namespaces_of`]. `local` is already known here for the same reason.
            let in_namespace = local.then(|| enclosing_namespaces_of(scopes, id)).flatten();

            for binding in &scope.bindings {
                if let Some(fact) = fact_for(
                    root,
                    &shapes,
                    binding,
                    prefix.clone(),
                    local,
                    in_namespace.clone(),
                    &declarations,
                ) {
                    facts.push(fact);
                }
            }
        }

        // **A declaration written with a qualified name** — `unsigned long CodeFormat::UpdateCodeStyle(…) { … }`.
        //
        // [`declared_name`] refuses these on purpose: `int ns::Widget::count = 0;` declares a member of `ns::Widget`,
        // and binding `count` in *this* file's scope would put a name in scope that cannot be used unqualified. The
        // cost of that refusal was larger than it looked: the walker made **no binding at all**, so the declaration
        // made **no fact**, so a `.cpp` definition was invisible to the whole index — Go to Definition from a call
        // site answered the header, Find References never showed the definition, and "go to implementation" on a
        // header declaration had nothing to find. Measured on a real project: `definitions("UpdateCodeStyle")` answered
        // **1** (the header), while the file defining it was open in front of the reader.
        //
        // The scope a qualified declaration belongs to is **written in the text**, so the fact is built from the
        // spelling — the class may be declared in a header this file's scope tree has never seen. No binding is added,
        // which keeps the lookup rule the refusal exists for.
        for (binding, scope, parameters) in qualified_declarations(root) {
            // **The walker may already have filed this declaration, with no scope.** A definition written in the
            // declarator — `std::vector<LuaTokenError> &LuaLexer::GetErrors() { … }` — is bound by the scope walker as a
            // plain function of this file, because the class is in a header the file only includes: its fact came out
            // with `scope: None`, so "go to implementation" on the header's `GetErrors` found nothing — it looks for the
            // same name whose scope is `LuaLexer`. The walker's fact is the better one (it has the parameters and the
            // return type), so this **adopts** it and supplies the scope instead of skipping it.
            if let Some(existing) = facts
                .iter_mut()
                .find(|fact| fact.name_range.start_offset == binding.name_range.start_offset)
            {
                if existing.scope.is_none() {
                    existing.scope = Some(scope);
                }
                // **And the range is the declaration's, not the declarator's.** The walker's binding covers the name
                // and its parameter list — `&LuaLexer::GetErrors()` — while "is this a definition" is asked of the
                // text: a declaration ends with `;`, a definition with `}`. With the declarator's range the answer was
                // "no" for every definition written this way, which is exactly what "go to implementation" asks.
                existing.range = binding.range;
                continue;
            }
            if let Some(mut fact) = fact_for(
                root,
                &shapes,
                &binding,
                Some(scope),
                false,
                None,
                &declarations,
            ) {
                // **The walker answered `None` for a definition's list**, because the shape it reads the declarator
                // from has no name (a qualified definition's name is in the specifier sequence). The pass read the
                // list from the declaration itself, so the fact a call site's signature is built from is complete.
                if fact.parameter_list.is_none() {
                    fact.parameter_list = parameters;
                }
                facts.push(fact);
            }
        }

        // The sweep below needs both lists in offset order. Facts are sorted rather than built in order
        // because the scope tree's order is depth-first, which is not offset order — a nested class's members
        // are walked before the next top-level declaration.

        facts.sort_by_key(|fact| fact.range.start_offset);

        // **The effects, in one pass over the file.** Asking each fact about its own body would walk the tree once per
        // declaration — measured on a folding fixture that includes `<string>` and `<vector>`: the server took longer
        // than a request's thirty-second budget to answer, because every inline function of those headers paid a walk
        // of the whole rendering.
        fill_the_frees(root, &mut facts);
        // **The reflection annotations, from the file text once per file** — `UCLASS(Blueprintable)` and
        // `UPROPERTY(EditAnywhere)` are empty macros, so nothing of them survives into the tree; the specifiers exist
        // only in the text, and a per-declaration read of it would copy the file once per fact.
        fill_the_reflection(root, &rendered, &mut facts);

        let mut targets: Vec<(&mut FactGuard, usize)> = facts
            .iter_mut()
            .map(|fact| (&mut fact.guard, fact.range.start_offset))
            .collect();
        assign_guards(&mut targets, preprocessing, &mut guards);

        (facts, guards)
    }
}

/// **Every declaration in this file written with a qualified name** — `void CodeFormat::UpdateCodeStyle(…) { … }`.
///
/// Returned as a [`Binding`] and the scope its spelling names, for [`DeclarationFacts::build`] to turn into a fact
/// through the same [`fact_for`] every other declaration goes through. It is a second pass rather than a change to the
/// scope walker because the walker's refusal is **right**: a qualified name declares nothing this file can use
/// unqualified, so no binding may be added. What was wrong was stopping there — a declaration with no binding made no
/// fact, and a `.cpp` definition was invisible to the whole index. Measured on a real project:
/// `definitions("UpdateCodeStyle")` answered **1** — the header's declaration — while the file defining it was the one
/// the reader had open.
///
/// The binding's `scope` field is [`ScopeId`]`(0)` and unused: [`fact_for`] takes the scope as its own argument, and the
/// scope a qualified declaration belongs to is the **spelling**, not a scope in this file's tree (the class is usually
/// in a header this tree has never seen).
fn qualified_declarations(root: &CppSyntaxNode) -> Vec<(Binding, String, Option<String>)> {
    use cpp_parser::{
        CppAstNode, CppAstToken, CppCompoundStat, CppDeclaration, CppDeclarator, CppNameExpr,
    };
    let mut found = Vec::new();

    for declaration in cpp_parser::typed_descendants::<CppDeclaration>(root) {
        // **Only a declaration outside every body.** A statement is not a declaration, and `ns::f(1);` inside a function body
        // parses as the *most vexing* declaration of a variable whose type is `ns::f` — without this guard the pass filed a
        // `Variable` named `f` in a file that only calls it.
        //
        // **A namespace body is a `CompoundStat` too**, and asking only for that kind skipped every qualified definition
        // written inside a namespace — the layout of every `.cpp` there is:
        //
        // ```text
        // namespace report {
        //     void Report::add(std::string line) { lines.push_back(std::move(line)); }
        // }
        // ```
        //
        // Measured on the reader's `report.cpp`, a module implementation unit: no `Function` fact existed for `add` or
        // `joined`, so Go to Implementation from `report.cppm` had nothing to find. What makes a body a body is the
        // *parent* of the compound: a function's hangs off the declaration, a branch's off the `if`, a namespace's off
        // the `NamespaceDecl` — a scope, not a body, so it is allowed through.
        if cpp_parser::typed_ancestors::<CppCompoundStat>(declaration.syntax()).any(|compound| {
            compound
                .syntax()
                .parent()
                .is_none_or(|parent| !cpp_parser::CppNamespaceDecl::can_cast(parent.kind().into()))
        }) {
            continue;
        }
        // **The declarator**, whichever of the two shapes the parser produced: under an `InitDeclarator`, or the declaration's
        // own child (a constructor's bare declarator).
        let Some(declarator) = declaration
            .get_init_declarator()
            .and_then(|init| init.get_declarator())
            .or_else(|| declaration.child::<CppDeclarator>())
        else {
            continue;
        };
        // **`void C::f()` writes the name as part of the type**, so the declarator is `()` and holds no name; a definition
        // written with the name in the declarator (`int C::count = 0`) has one.
        //
        // **The declarator's *own* name decides it**, and `get_name` is the reader that answers for every spelling: a direct
        // token (`&f()` holds `f` as a token), a direct `NameExpr` (`C::count`), and the nested wrappers in between. Taking
        // `child::<CppDeclarator>()` instead — the *first nested* declarator — looked at the `&` wrapper of `&f()`, which
        // holds no name, so the fallback below consulted the **specifiers**, and `std::string &f();` declared a function
        // called `string` in namespace `std` (`std::vector<int> &f();` one called `vector<int>`). Measured with
        // `inspect facts`; that bogus fact is what a hover on the return type's `string` showed, and what the semantic tokens
        // called a method.
        let names_itself = declarator.get_name().is_some();
        let from_the_declarator = declarator
            .child::<CppNameExpr>()
            .filter(CppNameExpr::is_qualified);
        let from_the_specifiers = (!names_itself)
            .then(|| {
                declaration.get_decl_specifiers().and_then(|specifiers| {
                    specifiers
                        .descendants::<CppNameExpr>()
                        .filter(CppNameExpr::is_qualified)
                        .last()
                })
            })
            .flatten();
        let name_node = match (from_the_declarator, from_the_specifiers) {
            (Some(found), _) => found,
            (None, Some(found)) => found,
            (None, None) => continue,
        };
        let spelling = name_node.get_qualified_name();
        let Some((qualifier, name)) = spelling.rsplit_once("::") else {
            continue;
        };
        let qualifier = qualifier.trim();
        let name = name.trim();
        if qualifier.is_empty() || name.is_empty() {
            continue;
        }

        // **The name's own range when it is an identifier**, and the whole spelling otherwise (a destructor and an operator have
        // no single identifier to highlight) — the same fallback `declared_name` makes, for the same reason: a fact whose
        // `name_range` covers nothing is a member row with no label.
        let name_range = name_node
            .get_identifier_token(name)
            .map(|token| cpp_parser::source_range(token.get_range()))
            .unwrap_or_else(|| cpp_parser::source_range(name_node.get_range()));

        let has_parameters = declaration.get_param_list().is_some();
        let class = qualifier.rsplit("::").next().unwrap_or(qualifier).trim();
        let kind = if name.starts_with('~') {
            crate::BindingKind::Destructor
        } else if name.starts_with("operator") {
            crate::BindingKind::OperatorFunction
        } else if name == class {
            crate::BindingKind::Constructor
        } else if has_parameters {
            crate::BindingKind::Function
        } else {
            crate::BindingKind::Variable
        };

        found.push((
            Binding {
                name: if let Some(rest) = name.strip_prefix('~') {
                    crate::Name::destructor(rest)
                } else if let Some(symbol) = name.strip_prefix("operator") {
                    crate::Name::operator(symbol.trim())
                } else {
                    crate::Name::identifier(name)
                },
                kind,
                range: crate::scopes::without_trailing_trivia(declaration.syntax()),
                name_range,
                scope: ScopeId(0),
                origin: None,
            },
            // **The scope is the *fully* qualified class**, not the spelling's own qualifier. A definition written inside
            // `namespace report` spells only `Report`, while the interface unit's declaration is filed under
            // `report::Report` — and a query that compares the two (`definitions_of`, which is what Go to Implementation
            // asks) sees two different classes. Measured on the reader's project: `report.cpp` carried `Report` and
            // `report.cppm` carried `report::Report`, so the definition was still unreachable.
            {
                let mut namespaces: Vec<String> = cpp_parser::typed_ancestors::<
                    cpp_parser::CppNamespaceDecl,
                >(declaration.syntax())
                .filter_map(|namespace| namespace.get_name())
                .map(|token| token.get_name_text().unwrap_or_default())
                .collect();
                namespaces.reverse();
                if !qualifier.is_empty() {
                    namespaces.push(qualifier.to_string());
                }
                namespaces.join("::")
            },
            // **The parameter list, taken from the declaration's own header** — the walker's declarator has no name for this
            // shape, so a fact read through it carried `parameter_list: None` and a signature had no fallback label.
            declaration
                .get_param_list()
                .map(|list| crate::inlay::tidy_list_text(&list.syntax().text().to_string()).0),
        ));
    }

    found
}
///
/// The one place that knows where the directives are, and therefore the one place that can answer this — which is
/// why it is a function rather than a step inside the declaration walk. **Every** kind of fact needs it: a
/// declaration, a `#define` and an `#include` can each be written inside an `#if`, and an include's guard is not
/// decoration — it is what decides whether the header's names are in scope at all.
///
/// A single pass over two sorted lists. The invariant is that both cursors only move forward, so a fact is
/// classified against the directives that ended before it and no others — which is exactly the question
/// `FilePreprocessing::guard_at` answers, asked once instead of once per fact.
///
/// `facts` must be sorted by offset. Regions are appended to `guards` as they are found, so the indices handed
/// out are only meaningful together with the list this fills.
pub fn assign_guards(
    facts: &mut [(&mut FactGuard, usize)],
    preprocessing: &FilePreprocessing,
    guards: &mut SummaryGuards,
) {
    // Built locally rather than written straight into `guards`, and the reason is the read below: the walk has to
    // ask where each open region *starts*, which is a shared borrow while a region is still being handed out. The
    // list is small — one entry per conditional directive — so the extra allocation is nothing next to having two
    // mutable borrows of the same field.
    let mut conditionals = conditionals(preprocessing);
    let regions = std::mem::take(&mut conditionals.regions);
    let conditions = std::mem::take(&mut conditionals.conditionals);
    let conditionals = conditionals.directives;

    let mut open: Vec<usize> = Vec::new();
    let mut cursor = 0usize;

    for (guard, at) in facts.iter_mut() {
        let at = *at;

        // Open and close every region decided before this fact.
        while cursor < conditionals.len() && conditionals[cursor].observed_by <= at {
            let region = conditionals[cursor];
            match region.close {
                false => open.push(region.index),
                true => {
                    // The region being closed is the innermost one, but the stack is searched rather than popped
                    // blindly: a malformed file can close a region it never opened, and popping whatever is on
                    // top would then attribute a fact to a region it is not in.
                    if let Some(position) = open.iter().rposition(|index| *index == region.index) {
                        open.truncate(position);
                    }
                }
            }
            cursor += 1;
        }

        // The innermost open region is the last one opened that also *starts* before the fact: a region whose
        // `#if` is written later cannot contain it, even if an `#endif` for something else has already been seen.
        let innermost = open
            .iter()
            .rev()
            .find(|index| regions[**index].start_offset <= at);

        **guard = match innermost {
            Some(index) => FactGuard::Region(*index as u32),
            None => FactGuard::Unconditional,
        };
    }
    guards.regions = regions;
    guards.conditionals = conditions;
}

/// Everything one walk over a file's conditionals knows: the regions, their shapes, and the sweep's directives.
struct Conditionals {
    /// The directives the guard sweep needs, in source order.
    directives: Vec<ConditionalDirective>,
    /// One entry per region: the span of its conditions, which is its identity in [`SummaryGuards`].
    regions: Vec<SourceRange>,
    /// One entry per region: the branches written for it and what encloses it. The **single** record of the
    /// file's conditional structure — the guard sweep hands the regions out from here, the settling rule reads
    /// the branches from here, and the summary stores this as it stands. Two structures would be two answers to
    /// "what does this region ask", which is the shape of bug this module's rules are written to avoid.
    conditionals: Vec<ConditionalRegion>,
    /// `#endif`s that closed nothing, and regions still open at the end of the file.
    ///
    /// Both are how a file whose directives do not balance is noticed — see [`mark_settling_macro_facts`], which
    /// refuses to apply its rule when either is non-zero.
    stray_closers: usize,
    unclosed: usize,
}

/// One conditional directive of the sweep, paired with the region it affects.
///
/// `observed_by` is the directive's **end**: a condition is not in force on the line it is written on, so `#if A`
/// must not be treated as open for a fact that starts before the directive finishes. That is the same rule
/// [`FilePreprocessing::guard_at`] applies, and it is the one that keeps a declaration on the `#if` line itself
/// outside the region it opens.
///
/// See [`conditionals`], which produces these alongside the region shapes both consumers need.
#[derive(Debug, Clone, Copy)]
struct ConditionalDirective {
    /// Which entry of `SummaryGuards::regions` this directive opens or closes.
    index: usize,
    /// Is this the `#endif` that ends the region, as opposed to the `#if` that began it?
    close: bool,
    /// The offset at which the directive is over, and therefore at which it takes effect.
    observed_by: usize,
}

/// One branch's last word on a name: which fact it is, and which way it settled the name.
///
/// A named pair rather than the tuple inline, because the two `usize`s and a kind in a nested map is a type a
/// reader has to decode every time they meet it.
type BranchFact = (usize, MacroKind);

/// What a branch's condition says about **one name**, when it says anything about a name at all.
///
/// The rule this exists for is about `#ifndef NAME / #define NAME`, so the only conditions that matter are the two
/// that name a macro's presence. Everything else is [`BranchCondition::Other`] and the rule makes no claim about
/// it — which is what keeps the rule from needing the condition evaluator, a macro environment, or a guess.
enum BranchCondition {
    /// `#ifndef NAME`, or `#if !defined(NAME)`.
    NotDefined(Box<str>),
    /// `#ifdef NAME`, or `#if defined(NAME)`.
    Defined(Box<str>),
    /// `#else`.
    Otherwise,
    /// Anything else — including a condition that did not read.
    Other,
}

/// What one stored branch's condition says about a name.
///
/// Read from the stored [`GuardBranch`] rather than from the directive, so that the settling rule and the query
/// that evaluates the condition later are reading the *same* record — if they read two, one of them would be
/// about a structure the other did not see. `defined`/`!defined` spellings go through
/// [`named_condition`], which reads tokens; `#ifdef`/`#ifndef` name their macro directly, so they need none.
fn branch_condition(branch: &GuardBranch) -> BranchCondition {
    match branch.kind {
        DirectiveKind::Ifndef => {
            BranchCondition::NotDefined(branch.condition.clone().unwrap_or_default())
        }
        DirectiveKind::Ifdef => {
            BranchCondition::Defined(branch.condition.clone().unwrap_or_default())
        }
        DirectiveKind::Else => BranchCondition::Otherwise,
        // `#if` and `#elif`, which is the only pair left: a condition is an expression there, and the two
        // spellings that name a macro are the four `defined` forms `named_condition` recognises.
        _ => named_condition(&branch.as_branch().tokens),
    }
}

/// Walk a file's conditionals once, producing the regions, their branches and the sweep's directives.
///
/// One walk for both consumers — [`assign_guards`], which hands the regions out as fact guards, and
/// [`mark_settling_macro_facts`], which reads the branches — because the two agree only if they read the same
/// structure, and two walks would be two chances to disagree about where a region ends. The structure it builds
/// is what a [`SummaryGuards`] stores, so a query evaluating a condition later reads the same record again.
fn conditionals(preprocessing: &FilePreprocessing) -> Conditionals {
    let mut found = Conditionals {
        directives: Vec::new(),
        regions: Vec::new(),
        conditionals: Vec::new(),
        stray_closers: 0,
        unclosed: 0,
    };
    // Where each currently-open region's index is, so an `#else` or `#endif` can find the region it continues or
    // closes without re-walking the directive list.
    let mut open: Vec<usize> = Vec::new();

    for spanned in &preprocessing.directives {
        let kind = spanned.directive.kind();

        if kind.opens_a_condition() {
            let index = found.regions.len();
            // The region's range is the condition itself, completed when the matching `#endif` is reached — see
            // `extend_region_to`. A consumer asking "was this compiled" re-reads this span, which is why the span
            // has to be the condition and not the whole region: the body can be megabytes and the question is
            // about the condition.
            found.regions.push(condition_span(spanned));
            found.conditionals.push(ConditionalRegion {
                parent: open.last().map(|index| *index as u32),
                branches: vec![conditional_branch(&spanned.directive, &spanned.range)],
                decoded: std::sync::OnceLock::new(),
            });
            open.push(index);
            found.directives.push(ConditionalDirective {
                index,
                close: false,
                observed_by: spanned.range.end_offset(),
            });
        } else if kind.closes_a_condition() {
            let Some(index) = open.last().copied() else {
                // An `#endif` with no `#if`: malformed, and the recovery is to record nothing rather than to
                // attribute the rest of the file to a region that does not exist. Counted, because it is also the
                // sign that the directive list is missing an opener.
                found.stray_closers += 1;
                continue;
            };

            // The directive ends the branch that was open: its body stops where this line begins. An `#else` or
            // `#elif` is followed by the next branch's body, an `#endif` is not.
            close_branch(&mut found.conditionals, index, spanned.range.start_offset);

            // `#else` and `#elif` end the region they were in and start another with the same identity: the
            // region *is* the conditional, and a fact on either branch is in the same `#if`. So the region's span
            // is extended and the region stays open.
            let closes = kind == DirectiveKind::Endif;
            extend_region_to(&mut found.regions, index, spanned.range.end_offset());

            if closes {
                open.pop();
                found.directives.push(ConditionalDirective {
                    index,
                    close: true,
                    observed_by: spanned.range.end_offset(),
                });
            } else {
                open_branch(
                    &mut found.conditionals,
                    index,
                    &spanned.directive,
                    &spanned.range,
                );
            }
        }
    }

    found.unclosed = open.len();
    found
}

/// Stop the open branch of `region` at `end`.
fn close_branch(conditionals: &mut [ConditionalRegion], region: usize, end: usize) {
    let Some(branch) = conditionals
        .get_mut(region)
        .and_then(|conditional| conditional.branches.last_mut())
    else {
        return;
    };

    branch.body = SourceRange::new(
        branch.body.start_offset,
        end.saturating_sub(branch.body.start_offset),
    );
}

/// Start the next branch of `region`.
fn open_branch(
    conditionals: &mut [ConditionalRegion],
    region: usize,
    directive: &Directive,
    range: &SourceRange,
) {
    let Some(conditional) = conditionals.get_mut(region) else {
        return;
    };

    conditional
        .branches
        .push(conditional_branch(directive, range));
}

/// The branch a conditional directive writes: what it asks, and where its body starts.
///
/// The body is empty here and completed by [`close_branch`] when the next branch's directive — or the `#endif` —
/// is reached, which is the only point at which its end is known.
fn conditional_branch(directive: &Directive, range: &SourceRange) -> GuardBranch {
    GuardBranch {
        kind: directive.kind(),
        condition: match directive {
            // The expression's tokens, spelled back into the text they were read from — **a space only where the
            // source had one**, which is what makes it that text and not a normalisation of it.
            //
            // Joining every token with a space is what this did, on the argument that "a space between tokens can
            // only ever *split* a token, never merge two, so re-reading the result gives the same expression".
            // That was true while the lexer produced maximal-munch tokens only, and it stopped being true when the
            // `>`-family began arriving in pieces (`CppLexer::tokenize`): `__cplusplus >= 201703L` joins to
            // `__cplusplus > = 201703L`, which re-reads as **two** tokens and no longer matches the
            // `__cplusplus >= 201703L` a configuration or a compile database carries.
            //
            // Adjacency is the whole test, and it needs no source text: two tokens that touch in the source were
            // one spelling there, and two that do not were separated by something.
            Directive::Conditional { condition, .. } => Some({
                let mut spelling = String::new();
                let mut previous_end = None;
                for token in condition.iter() {
                    if previous_end.is_some_and(|end| end != token.range.start_offset) {
                        spelling.push(' ');
                    }
                    spelling.push_str(token.text());
                    previous_end = Some(token.range.end_offset());
                }
                spelling.into()
            }),
            Directive::Ifdef { name, .. } => Some(name.clone()),
            // `#else` asks nothing, and says so with `None` rather than with an empty expression: an empty
            // condition is a syntax error in a preprocessor, and folding the two together would make
            // `#if` with nothing after it read as "always taken".
            _ => None,
        },
        body: SourceRange::new(range.end_offset(), 0),
        range: *range,
    }
}

/// `defined(NAME)`, `defined NAME`, `!defined(NAME)`, `!defined NAME` — and nothing else.
///
/// A **token pattern** rather than a parsed condition, on purpose: these four spellings are the whole of what the
/// rule needs, and running the condition parser over every `#if` in every file to look for them would be work no
/// answer uses. A condition that is anything else is `Other`, which is the answer that makes no claim.
fn named_condition(tokens: &[crate::token::Token]) -> BranchCondition {
    let significant: Vec<&crate::token::Token> = tokens
        .iter()
        .filter(|token| !crate::token::is_trivia(token.kind))
        .collect();
    let rest = without_outer_parentheses(&significant);

    let (negated, rest) = match rest.split_first() {
        Some((first, rest)) if first.kind == cpp_parser::CppTokenKind::LogicalNot => (true, rest),
        _ => (false, rest),
    };

    // `defined` is an ordinary identifier in the token stream — the lexer has no reason to know it.
    let Some((defined, rest)) = rest
        .split_first()
        .filter(|(token, _)| token.text() == "defined")
    else {
        return BranchCondition::Other;
    };
    let _ = defined;

    let parenthesised = rest
        .first()
        .is_some_and(|token| token.kind == cpp_parser::CppTokenKind::LeftParen);
    let rest = if parenthesised { &rest[1..] } else { rest };

    let Some((name, rest)) = rest
        .split_first()
        .filter(|(token, _)| token.is_identifier())
    else {
        return BranchCondition::Other;
    };

    let rest = if parenthesised {
        match rest.split_first() {
            Some((close, rest)) if close.kind == cpp_parser::CppTokenKind::RightParen => rest,
            _ => return BranchCondition::Other,
        }
    } else {
        rest
    };

    if !rest.is_empty() {
        return BranchCondition::Other;
    }

    if negated {
        BranchCondition::NotDefined(name.text.clone())
    } else {
        BranchCondition::Defined(name.text.clone())
    }
}

/// Strip parentheses that wrap the whole condition, so that `#if (!defined(N))` reads like `#if !defined(N)`.
fn without_outer_parentheses<'a>(
    tokens: &'a [&'a crate::token::Token],
) -> &'a [&'a crate::token::Token] {
    let mut rest = tokens;

    while rest.len() >= 2
        && rest[0].kind == cpp_parser::CppTokenKind::LeftParen
        && rest[rest.len() - 1].kind == cpp_parser::CppTokenKind::RightParen
    {
        // The first `(` has to match the last `)` — `(a) && (b)` also starts and ends with one, and it is not one
        // group.
        let mut depth = 0isize;
        let closes_at_the_end = rest.iter().enumerate().all(|(index, token)| {
            match token.kind {
                cpp_parser::CppTokenKind::LeftParen => depth += 1,
                cpp_parser::CppTokenKind::RightParen => depth -= 1,
                _ => {}
            }
            depth > 0 || index == rest.len() - 1
        });

        if !closes_at_the_end {
            break;
        }

        rest = &rest[1..rest.len() - 1];
    }

    rest
}

/// Mark the macro facts whose conditional **settles the name**, whatever branch is taken.
///
/// See [`crate::MacroFact::settles_the_name`] for what the flag claims and why the claim is sound. This is the
/// only place that can compute it: it needs the directives (for the branches), the regions (for the guards, which
/// `assign_guards` has already given each fact) and the facts themselves.
///
/// # The rule
///
/// A region settles a name when
///
/// ```text
/// every branch of it ends in a fact about that name, and all of them are the same kind, and
///   either it has an `#else` — one of its branches ran, and whichever it was, it acted on the name
///   or it is a single `#ifndef NAME` whose branch defines NAME — the condition is about the very name it writes
///   or it is a single `#ifdef NAME` whose branch undefines NAME — the mirror of the above
/// ```
///
/// **and every region it is nested inside settles the name too** — where a region that is the file's **own
/// include guard** counts as settling everything, for the reason [`crate::index`] gives when it de-guards those
/// facts: reaching the file's contents at all is what the guard means, so it is not a condition on anything. That
/// is what makes the rule reach a `#ifndef NAME` written inside a guarded header, which is where the shape
/// actually lives.
///
/// What a branch does to a name is decided by the **last** fact about it in that branch, so `#define N` followed
/// by `#undef N` settles "not a macro" — and a branch with no fact about the name at all settles nothing.
pub fn mark_settling_macro_facts(
    preprocessing: &FilePreprocessing,
    own_guard: Option<usize>,
    macros: &mut [MacroFact],
) {
    if macros.is_empty() {
        return;
    }

    let conditionals = conditionals(preprocessing);

    if conditionals.conditionals.is_empty() {
        return;
    }

    // **The nesting has to be the file's nesting.** A directive the parser did not produce — which a file with
    // syntax errors can lose — shifts the depth of every directive after it, and a parent chain that is wrong in
    // the *shorter* direction would let this rule claim a name is settled when an enclosing `#if` says otherwise.
    // An over-claim here is a wrong answer of the kind that gets a rename applied to code it should not touch, so
    // an unbalanced file gets no claims at all. Measured: `winnt.h` has 417 parse errors, they cost it eight
    // `#endif`s, and this is what notices.
    if conditionals.stray_closers > 0 || conditionals.unclosed > 0 {
        return;
    }

    // The last fact about each name directly inside each branch. "Directly" is the guard: a fact in a *nested*
    // region has that region's index, so it is not counted here — which is right, because whether it is reached is
    // the nested region's question.
    let mut last: HashMap<(usize, usize, &str), (MacroKind, usize)> = HashMap::new();

    for (index, fact) in macros.iter().enumerate() {
        let FactGuard::Region(region) = fact.guard else {
            continue;
        };

        let Some(shape) = conditionals.conditionals.get(region as usize) else {
            continue;
        };
        let Some(branch) = shape.branch_at(fact.range.start_offset) else {
            continue;
        };

        last.insert(
            (region as usize, branch, fact.name.as_str()),
            (fact.kind, index),
        );
    }

    // Grouped by region and name, so the rule below costs one pass over the facts rather than one per region.
    let mut mentioned: HashMap<usize, HashMap<&str, Vec<BranchFact>>> = HashMap::new();
    for (&(region, _, name), &(kind, index)) in &last {
        mentioned
            .entry(region)
            .or_default()
            .entry(name)
            .or_default()
            .push((index, kind));
    }

    // Outermost first, because a region's answer is its own rule **and** its parent's. Regions are numbered in
    // opening order, so a parent is always seen before its children.
    let mut settles: HashMap<(usize, &str), bool> = HashMap::new();
    let mut settling: Vec<usize> = Vec::new();

    for (region, shape) in conditionals.conditionals.iter().enumerate() {
        let Some(by_name) = mentioned.get(&region) else {
            continue;
        };

        for (name, in_branches) in by_name {
            if in_branches.len() != shape.branches.len() {
                continue;
            }

            let kind = in_branches[0].1;
            if in_branches.iter().any(|(_, held)| *held != kind) {
                continue;
            }

            // A single-branch region whose condition is about the very name it writes is the
            // `#ifndef NAME / #define NAME` idiom — and its mirror. Every other shape settles nothing, which is
            // what keeps this rule from needing the condition evaluator or a macro environment.
            let guaranteed = if shape.exhaustive() {
                true
            } else {
                match shape.branches.as_slice() {
                    [one] => match branch_condition(one) {
                        BranchCondition::NotDefined(condition) => {
                            condition.as_ref() == *name && kind.is_definition()
                        }
                        BranchCondition::Defined(condition) => {
                            condition.as_ref() == *name && !kind.is_definition()
                        }
                        BranchCondition::Otherwise | BranchCondition::Other => false,
                    },
                    _ => false,
                }
            };

            if !guaranteed {
                continue;
            }

            let inherited = shape.parent.is_none_or(|parent| {
                Some(parent as usize) == own_guard
                    || settles
                        .get(&(parent as usize, name))
                        .copied()
                        .unwrap_or(false)
            });

            settles.insert((region, name), inherited);

            if inherited {
                settling.extend(in_branches.iter().map(|(index, _)| *index));
            }
        }
    }

    for index in settling {
        macros[index].settles_the_name = true;
    }
}

/// Stretch a region's span to cover a directive that belongs to it.
///
/// The region is identified by its *span*, so a region whose span did not include its own `#endif` would be an
/// identity that changes as the file is read — and a resolver comparing spans would then fail to recognise two
/// facts as being in the same conditional.
fn extend_region_to(regions: &mut [SourceRange], index: usize, end: usize) {
    if let Some(region) = regions.get_mut(index) {
        let start = region.start_offset;
        let length = end.saturating_sub(start);
        *region = SourceRange::new(start, length);
    }
}

/// The span of a conditional directive's condition, which is the region's identity.
///
/// For `#if`/`#elif` that is the whole directive's span, because the condition's tokens are part of it. For
/// `#ifdef`/`#ifndef` it is the same span for the same reason. What matters is that it is a *stable* span: two
/// facts in one conditional must intern to one region, and a resolver must be able to re-read the text to
/// decide whether the branch is live.
fn condition_span(spanned: &SpannedDirective) -> SourceRange {
    match &spanned.directive {
        Directive::Conditional { .. } | Directive::Ifdef { .. } => spanned.range,
        // `#else` and `#endif` never open a region, so this arm is unreachable in practice; returning the
        // directive's own span keeps the function total rather than panicking on malformed input.
        _ => spanned.range,
    }
}

/// Index a file's facts by unqualified name, for the cheap "is this name declared anywhere" question.
///
/// A `HashMap` of `Vec` rather than of one fact, because a name may be declared many times in one file —
/// overloads, a member and a local — and choosing between them is resolution, which this layer does not do.
/// This is a *view* over the facts, built on demand and never stored: the summary keeps the flat list.
pub fn by_name(facts: &[DeclFact]) -> HashMap<&str, Vec<&DeclFact>> {
    let mut index: HashMap<&str, Vec<&DeclFact>> = HashMap::new();

    for fact in facts {
        index.entry(fact.name.as_str()).or_default().push(fact);
    }

    index
}

/// The scope a fact was declared in, as a [`ScopeId`] — the reverse of what [`build_facts`] stores.
///
/// The summary keeps the qualified *spelling* rather than an id, because an id is only meaningful inside the
/// tree it came from and the summary outlives that tree. A caller that still has the tree can map back with
/// this, which is what a rename needs: it edits the scope's binding, not the string.
pub fn scope_of<'a>(scopes: &'a ScopeTree, fact: &DeclFact) -> Option<(ScopeId, &'a Binding)> {
    let wanted = fact.scope.as_deref();

    scopes
        .scopes()
        .iter()
        .enumerate()
        .find_map(|(index, scope)| {
            if scope.name.as_deref() != wanted {
                return None;
            }

            let binding = scope
                .bindings
                .iter()
                .find(|binding| binding.name_range == fact.name_range)?;

            Some((ScopeId(index), binding))
        })
}

/// **The template parameters a declaration can name** — what keeps "not known yet" apart from "not here".
///
/// A parameter and a class of the same spelling are indistinguishable **in the type's text**, and
/// [`crate::sema::types::Type::depends_on_a_parameter`] answers only for a type that was *told* which names are
/// parameters. Nothing tells it for a member of a class template: [`DeclFact::type_of`] is the `Display`
/// rendering of a `Type` — `_Alloc` and `_Alloc::value_type` render exactly as a class of that name would — so
/// the question "is this type one only instantiation can finish" **cannot be asked of the summary at all**.
///
/// It can be asked of the scope tree, which is where the answer lives: `template <class _Alloc>` opens a
/// [`ScopeKind::TemplateParameters`] scope containing a [`BindingKind::TemplateParameter`] binding called
/// `_Alloc`, and everything the template declares is walked *inside* that scope — see
/// [`ScopeWalker::templated_declaration`](crate::sema::scopes), whose note says the declared name belongs
/// outside the parameter list while the contents belong within sight of it.
///
/// Measured on the closure of one file that includes `<format>`, `<vector>` and `<string>`: of 2129 type
/// spellings the reader could name, **1235 named something no lookup would answer** — `_Alloc::value_type`,
/// `_Alty_traits::difference_type`, `_AdaptorType::container_type` — and not one was a name the analysis had
/// failed to find. Recognising them brought that number down by an order of magnitude, and the ones left are
/// the subject of the next measurement rather than of this note.
///
/// The scope is found by **position**, not by name: a function body contributes no segment to a qualified name
/// (`template <class _Ty> void f(_Ty v)` puts `v` at `scope: None`) and a file scope is not a name at all, so a
/// lookup by spelling would miss the two cases that matter most. Ordered innermost-first, which is the order
/// [`ScopeTree::scope_chain`] walks in.
pub fn template_parameters_for<'a>(scopes: &'a ScopeTree, fact: &DeclFact) -> Vec<&'a str> {
    let Some(scope) = scopes.scope_at(fact.range.start_offset) else {
        return Vec::new();
    };

    let mut found: Vec<&str> = Vec::new();
    for id in scopes.scope_chain(scope) {
        let Some(data) = scopes.scope(id) else {
            continue;
        };
        if data.kind != ScopeKind::TemplateParameters {
            continue;
        }

        for binding in &data.bindings {
            if binding.kind == BindingKind::TemplateParameter
                && let Some(name) = binding.name.identifier_text()
            {
                found.push(name);
            }
        }
    }

    found
}
/// **Does this declaration's specifier sequence hold a directive line?** — see [`DeclFact::split_by_a_directive`].
///
/// The text is the one the written type was read from: `_GLIBCXX20_CONSTEXPR\n\treference\n#else\n\tvoid\n#endif` for
/// libstdc++'s `emplace_back`. A `#` at the start of a line inside it is the parser's own spelling of "this
/// declaration was written for two configurations", and it costs one `to_string` of a specifier node per declaration.
pub fn split_by_a_directive_with(shapes: &DeclarationShapes, binding: &Binding) -> bool {
    shapes
        .specifiers_at(the_offset_to_descend_by(binding))
        .map(|node| node.text().to_string())
        // **A *conditional* directive, and only a conditional one.** The flag means "the written reading states two
        // types and cannot know which is live", which is `#if`/`#ifdef`/`#ifndef`/`#elif`/`#else`/`#endif` and nothing
        // else. The first version asked whether *any* line of the specifier sequence starts with `#`, and a
        // `#pragma warning(push)` between the type and the name answered yes: measured on MSVC's `<vector>`, whose
        // rendering — which has every branch resolved — still carried the flag on `emplace_back`, so a hover asked a
        // second reading for a question the first had already answered.
        .is_some_and(|text| {
            text.lines().any(|line| {
                let Some(directive) = line.trim_start().strip_prefix('#') else {
                    return false;
                };
                let word: &str = directive
                    .trim_start()
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .next()
                    .unwrap_or_default();
                matches!(word, "if" | "ifdef" | "ifndef" | "elif" | "else" | "endif")
            })
        })
}

#[cfg(test)]
mod tests {
    use super::{build_facts, by_name, template_parameters_for};
    use crate::preprocess::preprocess;
    use crate::sema::scopes::build_scopes;
    use crate::summary::DeclFact;
    use crate::summary::{DeclKind, FactGuard};
    use cpp_parser::{CppParser, ParserConfig};

    /// **`export` governs two spellings, and the namespace one is the case that hid a whole library.**
    ///
    /// `export int f();` puts the keyword inside the declaration it exports; `export namespace n { … }` puts it
    /// **beside** the namespace, as a sibling token. A reader that only looked for the first spelling found nothing
    /// exported in the fixture's interface unit (`tests/fixtures/modules`), and a file that says `import mathlib;`
    /// could name none of its members.
    ///
    /// Both directions in one fixture, because the mistake this replaces was in the *hiding* direction too: a
    /// namespace declared again **without** `export` must not inherit its neighbour's.
    #[test]
    fn a_declaration_is_exported_only_where_export_reaches_it() {
        let source = "export module m;\n\
                      export namespace ns { int inside(); }\n\
                      namespace ns { int outside(); }\n\
                      export int alone();\n\
                      int plain();\n";
        let (facts, _) = facts(source);

        let exported = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("{name} must be a fact"))
                .exported
        };

        assert!(exported("inside"), "inside an exported namespace");
        assert!(
            !exported("outside"),
            "the second namespace has no `export`, so a declaration in it is not exported"
        );
        assert!(exported("alone"), "`export int alone();`");
        assert!(
            !exported("plain"),
            "and a declaration with no `export` above it"
        );
    }

    /// **A parameter of an enclosing template is visible to everything inside it, and to nothing outside.**
    ///
    /// This is the predicate that keeps "not known yet" apart from "not here", and it has to be asked of the
    /// **scope tree** rather than of the type: [`DeclFact::type_of`] is the `Display` rendering of a `Type`, so
    /// `_Alloc` and a class named `_Alloc` are the same text, and `_Alloc::value_type` is a name no lookup can
    /// ever answer.
    ///
    /// The three cases are the three the walk can get wrong: a member of a **class** template, a parameter of a
    /// **function** template — which lives at `scope: None`, because a function body contributes no segment to a
    /// qualified name — and the class's own name, which is declared **outside** the parameter list on purpose
    /// (`template <typename T> class Array` declares `Array` in the enclosing scope and `T` within sight of it).
    #[test]
    fn a_declaration_inside_a_template_can_name_its_parameters() {
        let source = "template <class _Alloc>\n\
                      struct _Container {\n\
                          using value_type = typename _Alloc::value_type;\n\
                      };\n\
                      template <class _Ty> void take(_Ty value) { }\n";

        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.get_errors(), [], "the input must parse cleanly");

        let root = tree.get_red_root();
        let scopes = build_scopes(&root, &crate::NoMacroBodies);
        let (facts, _) = build_facts(&scopes, &preprocess(source, tree.get_tokens()), &root, &[]);

        let parameters_of = |name: &str| -> Vec<String> {
            let fact = facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("{name} must be a fact"));

            template_parameters_for(&scopes, fact)
                .into_iter()
                .map(str::to_string)
                .collect()
        };

        assert_eq!(
            parameters_of("value_type"),
            vec!["_Alloc".to_string()],
            "a member of a class template names the class's own parameter"
        );
        assert_eq!(
            parameters_of("take"),
            vec!["_Ty".to_string()],
            "and a function template's parameter is in scope in its body, where the fact's `scope` is `None`"
        );
        assert!(
            !parameters_of("_Container").contains(&"_Container".to_string()),
            "the class's own name is not one of its parameters, so a type spelled `_Container` is not \
             dependent. The list is not empty, and that is right rather than a leak: a template's parameters \
             are in scope for the declaration that follows, **its own name included** — the note on \
             `templated_declaration` says the name belongs to the enclosing scope, which is a statement about \
             where the *binding* goes and not about what can be named. What the filter compares is the name, \
             and `_Container` is not `_Alloc`: got {:?}",
            parameters_of("_Container")
        );
    }

    /// **An alias declares a fact, in every shape a header writes one.**
    ///
    /// Measured on the closure of one file that includes `<format>`, `<vector>` and `<string>`: `_Alloc_ptr_t`,
    /// `_Alloc_size_t` and `_Alty_traits` are held by the index **zero** times — not a lookup that fails, but a
    /// declaration that never became a fact — while `std::basic_string::size_type`, also an alias, resolves.
    /// So the question is which *shape* of alias is read, and the four shapes are here:
    ///
    /// ```text
    /// using plain = int;                                 file scope
    /// template <class T> using ptr = …;                  namespace scope, and a **template**
    /// using size_type = unsigned long long;              a class member, and a concrete type
    /// using traits = allocator_traits<int>;              a class member, and a **template-id**
    /// ```
    ///
    /// The last two are the pair that matters: `basic_string::size_type` is the third shape and it is in the
    /// index, so if the fourth is not, the difference is the template-id and not the member.
    #[test]
    fn an_alias_declares_a_fact_in_every_shape() {
        let source = "using plain = int;\n\
                      namespace std {\n\
                          template <class _Alloc>\n\
                          using _Alloc_ptr_t = typename allocator_traits<_Alloc>::pointer;\n\
                          struct _Guard {\n\
                              using size_type = unsigned long long;\n\
                              using _Alty_traits = allocator_traits<int>;\n\
                          };\n\
                      }\n";

        let (facts, _) = facts(source);

        // The reading, so that a failure says what was found rather than only what was not.
        let everything: Vec<(String, DeclKind)> = facts
            .iter()
            .map(|fact| (fact.qualified_name(), fact.kind))
            .collect();
        let named = |name: &str| -> Option<String> {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .map(DeclFact::qualified_name)
        };

        assert_eq!(
            named("plain").as_deref(),
            Some("plain"),
            "a file-scope alias is a declaration: {everything:?}"
        );
        assert_eq!(
            named("_Alloc_ptr_t").as_deref(),
            Some("std::_Alloc_ptr_t"),
            "a namespace-scope alias **template** is a declaration: {everything:?}"
        );
        assert_eq!(
            named("size_type").as_deref(),
            Some("std::_Guard::size_type"),
            "a member alias to a concrete type is a declaration: {everything:?}"
        );
        assert_eq!(
            named("_Alty_traits").as_deref(),
            Some("std::_Guard::_Alty_traits"),
            "a member alias to a **template-id** is a declaration — this is the shape that was missing, and \
             `size_type` above is the control that says the member half works: {everything:?}"
        );
    }

    /// **A base clause is read, including when a macro stands between the keyword and the name.**
    ///
    /// `struct Widget : Base { … };` is the plain shape. The one that matters is MSVC's, measured:
    ///
    /// ```cpp
    /// _EXPORT_STD template <class _Alloc>
    /// struct _NO_SPECIALIZATIONS_CITING("N5014 …") allocator_traits
    ///     : conditional_t<…, _Default_allocator_traits<_Alloc>, _Normal_allocator_traits<_Alloc>> {};
    /// ```
    ///
    /// (`xmemory:755`) — an attribute-like macro the index cannot expand sits **between `struct` and the
    /// class's name**, and the base clause follows. [`DeclFact::bases`] is what a member lookup walks into, so a
    /// base that is not recorded is a member that cannot be found: measured, `std::allocator_traits` resolves
    /// and `std::allocator_traits::pointer` — declared in one of those two bases — answers `NotDeclaredHere`,
    /// while `std::_Normal_allocator_traits::pointer` resolves.
    ///
    /// The base is recorded **as written**, arguments and all, because that is what the clause says: turning
    /// `_Normal_allocator_traits<_Alloc>` into a class is the reader's job, and `conditional_t<…>` is not a
    /// class name at all — see `ProjectIndex::bases_of_a_class`.
    #[test]
    fn a_base_clause_survives_a_macro_before_the_name() {
        let source = "struct Base { int b; };\n\
                      struct _Attr(\"x\") Widget : Base { int w; };\n\
                      struct Plain : Base { int p; };\n\
                      template <class _Alloc>\n\
                      struct _Attr(\"y\") Traits\n\
                          : conditional_t<true, Other<_Alloc>, Base> {};\n\
                      _EXPORT_STD template <class _Alloc>\n\
                      struct _Attr(\"z\") Exported\n\
                          : conditional_t<true, Other<_Alloc>, Base> {};\n\
                      template <class _Alloc>\n\
                      struct [[msvc::no_specializations(\"a\" \"b\")]] Attributed\n\
                          : conditional_t<true, Other<_Alloc>, Base> {};\n\
                      template <class _Alloc>\n\
                      struct [[msvc::no_specializations(\"Specializing this standard library template is forbidden by \" \"N5014 [allocator.traits.general]/1\")]] Bracketed\n\
                          : conditional_t<true, Other<_Alloc>, Base> {};\n\
                      namespace ns {\n\
                      template <class _Alloc>\n\
                      struct [[msvc::no_specializations(\"a\" \"b\")]] Nested\n\
                          : conditional_t<true, Other<_Alloc>, Base> {};\n\
                      }\n\
                      template <class _Alloc>\n\
                      struct [ [ msvc :: no_specializations ( \"a\" ) ] ] Spaced\n\
                          : conditional_t<true, Other<_Alloc>, Base> {};\n";

        let (facts, _) = facts(source);
        let bases_of = |name: &str| -> Vec<String> {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("{name} must be a fact"))
                .bases
                .clone()
        };

        assert_eq!(
            bases_of("Widget"),
            vec!["Base".to_string()],
            "a macro between the keyword and the name does not change what the class derives from"
        );
        assert_eq!(
            bases_of("Widget"),
            bases_of("Plain"),
            "…which is the same answer the plain spelling gives"
        );
        assert_eq!(
            bases_of("Traits"),
            vec!["conditional_t<true, Other<_Alloc>, Base>".to_string()],
            "and the clause is kept **as written** — `conditional_t<…>` is not a class name, and turning it \
             into one is not this layer's question"
        );
        // **And a macro in front of the whole declaration**, which is how `_EXPORT_STD` is written. It expands
        // to `export` in one configuration and to nothing in another, and neither reading is available to a
        // file being parsed on its own — so the declaration has to be read with the macro standing there.
        assert_eq!(
            bases_of("Exported"),
            vec!["conditional_t<true, Other<_Alloc>, Base>".to_string()],
            "a macro before `template` does not stop the class from being declared: {:?}",
            facts
                .iter()
                .map(|fact| fact.qualified_name())
                .collect::<Vec<_>>()
        );
        // **The attribute as the rendering actually spells it.** `_NO_SPECIALIZATIONS_CITING(...)` is
        // `_NO_SPECIALIZATIONS_MSG(...)`, which is `[[msvc::no_specializations(_Msg)]]` in one arm of its `#if`
        // and **nothing** in another — so the file the analysis reads may hold either, and the two are not the
        // same shape: the macro unexpanded is an identifier with arguments, the attribute is real syntax
        // standing between `struct` and the name. Measured on `<vector>`'s closure, the rendering of
        // `xmemory:755` is this spelling, and `allocator_traits` has **no definition fact at all** — only the
        // forward declaration without a base clause, which is why `std::allocator_traits::pointer` cannot be
        // found.
        assert_eq!(
            bases_of("Attributed"),
            vec!["conditional_t<true, Other<_Alloc>, Base>".to_string()],
            "an attribute between `struct` and the name does not stop the class from being declared: {:?}",
            facts
                .iter()
                .map(|fact| fact.qualified_name())
                .collect::<Vec<_>>()
        );
        // …and the attribute's **argument as the header writes it**: a string containing `[`, `]` and `/`.
        assert_eq!(
            bases_of("Bracketed"),
            vec!["conditional_t<true, Other<_Alloc>, Base>".to_string()],
            "the text inside the attribute's string does not change how the declaration is read: {:?}",
            facts
                .iter()
                .map(|fact| fact.qualified_name())
                .collect::<Vec<_>>()
        );
        // …and the same declaration **inside a namespace**, which is where every one of the standard library's
        // is written. Measured on `<vector>`'s closure, `xmemory:755` declares `allocator_traits` inside
        // `namespace std` and produces **no fact for the name at all** — only one for its template parameter
        // `_Alloc` — while `allocator_traits` at file scope above is declared normally.
        assert_eq!(
            bases_of("Nested"),
            vec!["conditional_t<true, Other<_Alloc>, Base>".to_string()],
            "a namespace around the declaration changes nothing about it: {:?}",
            facts
                .iter()
                .map(|fact| fact.qualified_name())
                .collect::<Vec<_>>()
        );
        // **And the attribute as the rendering spells it — with a space between the brackets.**
        //
        // This is the shape the product actually parses: `_NO_SPECIALIZATIONS_MSG(_Msg)` expands to
        // `[[msvc::no_specializations(_Msg)]]`, and the renderer writes one space between every pair of tokens,
        // so what reaches the parser is `[ [ msvc :: no_specializations ( "…" ) ] ]`. Measured on `<vector>`'s
        // closure, the rendering of `xmemory:755` is exactly this, and that declaration produces **no fact at
        // all** while every tightly-spelled variant above produces one.
        //
        // The same family as `>>` becoming `> >`, which the renderer also causes: an adjacency the parser reads
        // as syntax is not adjacency once there is a space in it.
        assert_eq!(
            bases_of("Spaced"),
            vec!["conditional_t<true, Other<_Alloc>, Base>".to_string()],
            "a space between the attribute's brackets does not change what the declaration is: {:?}",
            facts
                .iter()
                .map(|fact| fact.qualified_name())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_alias_records_the_type_it_points_at() {
        // The two spellings a C++ alias comes in, and the function-pointer shape where the specifiers alone are
        // not the type.
        let source = "using A = basic_string<char>;\n\
                      typedef basic_string<char> B;\n\
                      typedef void (*F)(int);\n\
                      typedef int int32_t;\n";
        let (facts, _) = facts(source);

        let type_of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("{name} must be a fact"))
                .type_of
                .clone()
        };

        assert_eq!(type_of("A").as_deref(), Some("basic_string<char>"));
        assert_eq!(type_of("B").as_deref(), Some("basic_string<char>"));
        assert_eq!(
            type_of("F").as_deref(),
            Some("void (*)(int)"),
            "the specifiers plus the declarator with the alias's own name cut out"
        );
        assert_eq!(type_of("int32_t").as_deref(), Some("int"));
    }

    #[test]
    fn a_member_whose_type_is_preceded_by_macros_is_named_by_its_declarator() {
        // The shape every standard-library method has, and the one that made `std::basic_string` index 117
        // members with no method among them:
        //
        // ```cpp
        // _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR
        // size_type
        // size() const _GLIBCXX_NOEXCEPT;
        // ```
        //
        // Two macros stand where a declaration specifier goes, so the specifier sequence holds **three** names and
        // the declared name is in the declarator after them. A reader that takes the last name of the specifier
        // sequence binds `size_type` — a name that is also a real member, so the mistake does not even look wrong
        // until a query asks for `size` and finds nothing.
        let source = "struct S {\n  _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR\n  size_type\n  size() const noexcept;\n};\n";
        let (declared, _) = facts(source);

        let names: Vec<&str> = declared.iter().map(|fact| fact.name.as_str()).collect();
        assert!(
            names.contains(&"size"),
            "the member is named by its declarator: {names:?}"
        );
        assert!(
            !names.contains(&"size_type")
                || names.iter().filter(|name| **name == "size_type").count() == 1,
            "and no member is named after the type: {names:?}"
        );

        // The same member with an inline **body**, which is how `basic_string::size` is really written, and with a
        // directive block before it — the shape the class body's member loop had to learn to walk through.
        let defined = "struct S {\n#if FEATURE\n  int a;\n#endif\n  _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR\n  size_type\n  size() const noexcept\n  { return 0; }\n};\n";
        let (defined_facts, _) = facts(defined);

        let names: Vec<&str> = defined_facts
            .iter()
            .map(|fact| fact.name.as_str())
            .collect();
        assert!(
            names.contains(&"size"),
            "an inline definition is named the same way as a declaration: {names:?}"
        );

        // …and the exact suffix `basic_string::size` writes: `_GLIBCXX_NOEXCEPT` is a **macro** standing where
        // `noexcept` would go, which is the other half of the "a macro where a specific token is expected" family.
        let suffix_macro = "struct S {\n  _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR\n  size_type\n  size() const _GLIBCXX_NOEXCEPT\n  { return 0; }\n};\n";
        let (suffix_facts, _) = facts(suffix_macro);

        let names: Vec<&str> = suffix_facts.iter().map(|fact| fact.name.as_str()).collect();
        assert!(
            names.contains(&"size"),
            "a macro in the function-suffix position does not rename the member: {names:?}"
        );
    }

    #[test]
    fn a_class_records_no_type_of_its_own() {
        // The rule that tells an alias from a class in a stored fact, asserted where it is produced: a class *is*
        // a type rather than pointing at one, and `type_of` is what a consumer reads to follow an alias.
        let (facts, _) = facts("struct Widget { int size; };\nusing Alias = Widget;\n");
        let widget = facts
            .iter()
            .find(|fact| fact.name == "Widget")
            .expect("the class is a fact");

        assert_eq!(widget.type_of, None);
        assert_eq!(widget.kind, DeclKind::Type);
    }

    /// **A name written with template arguments in its qualifier** — `Box<int>::grow`, `Box<int>::count`,
    /// `Box<int>::Inner::deep` — is declared **in that class**, and the name it declares is the last segment.
    ///
    /// The template arguments are the whole difficulty, and they break three separate readings at once, each
    /// silently:
    ///
    /// ```text
    /// the scope's name        a class's scope is called `Box`, never `Box<int>`, so a lookup for `Box<int>::Inner`
    ///                         finds nothing and the member is attributed to the file
    /// the separator           `<` and `>` nest (`Box<A::B>::grow`), so the last `::` is not the last two characters
    ///                         before the name — a split on the text cuts inside an argument list
    /// the declared name       `Box<int>::Inner::deep`'s name is `deep`, and it is written in the *specifier
    ///                         sequence* rather than in a declarator, so a reader looking in the declarator finds
    ///                         nothing
    /// ```
    ///
    /// Measured before this was handled: `int Box<int>::count = 0;` declared **no fact at all**, and the two
    /// `grow` definitions produced facts whose scope was `Box` but with the body walked at file scope.
    #[test]
    fn a_qualified_name_with_template_arguments_declares_in_that_class() {
        let source = "\
template <class T> class Box {
public:
    int size;
    void grow(int n);
    struct Inner { int deep; };
};
void Box<int>::grow(int n) { size = n; }
int Box<int>::count = 0;
int Box<int>::Inner::deep = 0;
";
        let (facts, _) = facts(source);

        let scope_of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("`{name}` must be a fact: {facts:?}"))
                .scope
                .clone()
        };

        assert_eq!(scope_of("grow").as_deref(), Some("Box"));
        assert_eq!(
            scope_of("count").as_deref(),
            Some("Box"),
            "`int Box<int>::count = 0;` is a member of `Box` — it used to declare nothing at all"
        );
        assert_eq!(
            scope_of("deep").as_deref(),
            Some("Box::Inner"),
            "and a nested class's member is qualified by the *names*, without the template arguments"
        );

        // The name is the last segment, never the qualified spelling: a fact called `Box<int>::count` would not be
        // found by any lookup, and would be a second entry for a class that has one.
        assert!(
            !facts.iter().any(|fact| fact.name.contains("::")),
            "no fact is named with a qualifier"
        );
    }

    fn facts(source: &str) -> (Vec<crate::summary::DeclFact>, crate::summary::SummaryGuards) {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.get_errors(), [], "the input must parse cleanly");

        let root = tree.get_red_root();
        build_facts(
            &build_scopes(&root, &crate::NoMacroBodies),
            &preprocess(source, tree.get_tokens()),
            &root,
            &[],
        )
    }

    /// **The reflection annotation a declaration was written under**, with its specifiers as the file spelled them.
    ///
    /// `UCLASS` and `UPROPERTY` expand to nothing, so the tree a fact is built from holds neither: the specifiers survive
    /// only in the text, and `fill_the_reflection` reads them back from the declaration's own offset. This pins the
    /// shapes a rule and a hover both need — a macro with bare specifiers, one with a value, and one whose value has
    /// commas inside it (`meta=(…)`), which must stay **one** specifier.
    #[test]
    fn a_reflection_annotation_is_recorded_with_its_specifiers() {
        let source = "\
UCLASS(Blueprintable, BlueprintType)
class MYGAME_API AMyActor
{
public:
    UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = \"Game\")
    bool bIsReady = false;

    UFUNCTION(BlueprintCallable, meta=(DisplayName=\"Do It\", ToolTip=\"a, b\"))
    void DoThing();

    int32 NotAnnotated = 0;
};
";
        let (facts, _) = facts(source);

        let class = facts
            .iter()
            .find(|fact| fact.name == "AMyActor")
            .expect("the class");
        let reflection = class.reflection.clone().expect("UCLASS");
        assert_eq!(reflection.macro_name, "UCLASS");
        assert_eq!(
            reflection.names().collect::<Vec<_>>(),
            vec!["Blueprintable", "BlueprintType"]
        );

        let member = facts
            .iter()
            .find(|fact| fact.name == "bIsReady")
            .expect("the member");
        let reflection = member.reflection.clone().expect("UPROPERTY");
        assert_eq!(reflection.macro_name, "UPROPERTY");
        assert_eq!(
            reflection.specifiers,
            vec!["EditAnywhere", "BlueprintReadWrite", "Category = \"Game\"",]
        );
        assert_eq!(reflection.value_of("Category"), Some("\"Game\""));

        let function = facts
            .iter()
            .find(|fact| fact.name == "DoThing")
            .expect("the function");
        let reflection = function.reflection.clone().expect("UFUNCTION");
        assert_eq!(reflection.macro_name, "UFUNCTION");
        assert_eq!(
            reflection.specifiers,
            vec![
                "BlueprintCallable",
                "meta=(DisplayName=\"Do It\", ToolTip=\"a, b\")",
            ],
            "a value with commas in it is one specifier"
        );

        let plain = facts
            .iter()
            .find(|fact| fact.name == "NotAnnotated")
            .expect("the plain member");
        assert!(plain.reflection.is_none());
    }
    /// The facts of a file that **does not** parse cleanly, with the diagnostics that say so.
    fn facts_of_a_broken_file(
        source: &str,
    ) -> (Vec<crate::summary::DeclFact>, Vec<cpp_parser::SourceRange>) {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_ne!(
            tree.get_errors(),
            [],
            "this fixture is meant to have errors: {source:?}"
        );

        let root = tree.get_red_root();
        let errors: Vec<cpp_parser::SourceRange> = tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();

        (
            build_facts(
                &build_scopes(&root, &crate::NoMacroBodies),
                &preprocess(source, tree.get_tokens()),
                &root,
                &errors,
            )
            .0,
            errors,
        )
    }

    fn qualified(source: &str) -> Vec<String> {
        let (facts, _) = facts(source);
        let mut names: Vec<String> = facts
            .iter()
            .map(|fact| match &fact.scope {
                Some(scope) => format!("{scope}::{}", fact.name),
                None => fact.name.clone(),
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_top_level_declaration_has_no_scope_prefix() {
        assert_eq!(qualified("int count;\n"), ["count"]);
    }

    #[test]
    fn a_member_is_qualified_by_its_class_and_its_namespaces() {
        assert_eq!(
            qualified("namespace ns { struct C { int member; void method(); }; }\n"),
            ["ns", "ns::C", "ns::C::member", "ns::C::method"]
        );
    }

    #[test]
    fn the_nested_namespace_spellings_agree() {
        let compact = qualified("namespace a::b { struct C { int member; }; }\n");
        let spelled = qualified("namespace a { namespace b { struct C { int member; }; } }\n");
        assert_eq!(compact, spelled);
    }

    #[test]
    fn a_function_body_does_not_qualify_its_locals() {
        let names = qualified("namespace ns { void f() { int local = 0; } }\n");

        assert!(
            names.contains(&"local".to_string()),
            "the local is declared and must be a fact: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name.contains("ns::f::")),
            "a function body contributes no segment to a qualified name: {names:?}"
        );
    }

    #[test]
    fn facts_inside_a_conditional_carry_its_region() {
        let source = "int outside;\n#if defined(A)\nint inside;\n#endif\nint after;\n";
        let (facts, guards) = facts(source);

        assert_eq!(guards.regions.len(), 1, "one `#if`, one region");

        let guard_of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("{name} must be a fact"))
                .guard
        };

        assert_eq!(guard_of("outside"), FactGuard::Unconditional);
        assert_eq!(guard_of("inside"), FactGuard::Region(0));
        assert_eq!(
            guard_of("after"),
            FactGuard::Unconditional,
            "the region is closed by its own `#endif`"
        );
    }

    #[test]
    fn a_declaration_inside_a_region_is_guarded_and_the_region_stays_open_until_endif() {
        let source = "#if defined(A)\nint inside;\nint also_inside;\n#endif\nint after;\n";
        let (facts, guards) = facts(source);

        assert_eq!(guards.regions.len(), 1);

        let guard_of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("{name} must be a fact"))
                .guard
        };

        assert_eq!(guard_of("inside"), FactGuard::Region(0));
        assert_eq!(
            guard_of("also_inside"),
            FactGuard::Region(0),
            "a second declaration in the same region interns to the same one"
        );
        assert_eq!(guard_of("after"), FactGuard::Unconditional);
    }

    /// A declaration written on the *same line* as `#if` is inside the directive, and therefore not a fact.
    ///
    /// Pinned because it looks like a miss and is not: `#if` consumes the rest of its line, so the parser reads
    /// `#if defined(A) int inside;` as one `PreprocessorDirective` node holding every token — which is what a
    /// compiler does too, since the line is the directive's argument. There is no declaration to index because
    /// there is no declaration in the C sense either.
    ///
    /// The test exists so that a future change to the directive rule, which would start producing a
    /// `Declaration` here, is a deliberate edit rather than a silent difference in what the index holds.
    #[test]
    fn a_declaration_on_the_directive_line_is_not_a_fact() {
        let source = "#if defined(A) int inside;\n#endif\nint outside;\n";
        let (facts, guards) = facts(source);

        assert_eq!(guards.regions.len(), 1, "the `#if` still opens a region");
        assert!(
            !facts.iter().any(|fact| fact.name == "inside"),
            "the whole line is the directive's argument: {:?}",
            facts.iter().map(|fact| &fact.name).collect::<Vec<_>>()
        );
        assert!(
            facts.iter().any(|fact| fact.name == "outside"),
            "and the declaration after the region is unaffected"
        );
    }

    #[test]
    fn nested_conditionals_give_the_innermost_region() {
        let source = "#if defined(A)\nint outer;\n#if defined(B)\nint inner;\n#endif\n#endif\n";
        let (facts, guards) = facts(source);

        assert_eq!(guards.regions.len(), 2, "each `#if` is its own region");

        let guard_of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("{name} must be a fact"))
                .guard
        };

        assert_ne!(guard_of("outer"), guard_of("inner"));
        assert!(
            matches!(guard_of("inner"), FactGuard::Region(_)),
            "the inner declaration is guarded by the inner region"
        );
    }

    #[test]
    fn an_unbalanced_endif_does_not_swallow_the_rest_of_the_file() {
        // Malformed input, which this layer runs on by design. The recovery is that the `#endif` is ignored
        // rather than that everything after it is attributed to a region that was never opened.
        let source = "#endif\nint after;\n";
        let (facts, guards) = facts(source);

        assert!(guards.regions.is_empty(), "no region was opened");
        assert_eq!(
            facts
                .iter()
                .find(|fact| fact.name == "after")
                .expect("the declaration is a fact")
                .guard,
            FactGuard::Unconditional
        );
    }

    #[test]
    fn the_by_name_view_groups_a_name_declared_twice() {
        let (facts, _) = facts("void f();\nvoid f(int);\n");
        let index = by_name(&facts);

        assert_eq!(index.get("f").map(Vec::len), Some(2), "both overloads");
        assert_eq!(index.get("g"), None, "a name no declaration mentions");
    }

    #[test]
    fn a_declaration_kind_survives_the_walk() {
        let (facts, _) = facts("struct C { int member; void method(); };\nnamespace ns { }\n");

        let kind_of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .map(|fact| fact.kind)
        };

        assert_eq!(kind_of("C"), Some(DeclKind::Type));
        assert_eq!(kind_of("member"), Some(DeclKind::Variable));
        assert_eq!(kind_of("method"), Some(DeclKind::Function));
        assert_eq!(kind_of("ns"), Some(DeclKind::Namespace));
    }

    /// The names of the facts of a file that does not parse cleanly whose declaration was touched.
    fn unclean(source: &str) -> Vec<String> {
        let (facts, errors) = facts_of_a_broken_file(source);
        assert!(!errors.is_empty(), "the fixture must have diagnostics");

        facts
            .iter()
            .filter(|fact| !fact.clean)
            .map(|fact| fact.name.clone())
            .collect()
    }

    #[test]
    fn a_file_that_parses_cleanly_is_clean() {
        let (facts, _) = facts("struct S { int a; };\nint count;\nvoid f() { int local; }\n");

        assert!(!facts.is_empty(), "the fixture declares something");
        assert!(
            facts.iter().all(|fact| fact.clean),
            "with no diagnostics there is nothing to be unclean about: {:?}",
            facts
                .iter()
                .filter(|fact| !fact.clean)
                .map(|fact| fact.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_broken_declaration_leaves_the_rest_of_its_file_alone() {
        // The declaration that failed contributes **no fact at all** — which is what every fixture tried while
        // choosing this rule did, and worth pinning: a reading that goes wrong usually removes the fact rather
        // than producing a wrong one. What the field is for is the facts around it, and they stay clean.
        let (facts, errors) = facts_of_a_broken_file("struct S { int a; };\nint b = ;\n");

        assert_eq!(errors.len(), 1, "one error, in the second declaration");
        assert_eq!(
            facts
                .iter()
                .map(|fact| fact.name.as_str())
                .collect::<Vec<_>>(),
            ["S", "a"],
            "no fact for the declaration that failed"
        );
        assert!(
            facts.iter().all(|fact| fact.clean),
            "and the two that were read are untouched by the recovery"
        );
    }

    #[test]
    fn an_error_inside_a_declaration_is_reported_on_it() {
        // Both fixtures keep a fact for the declaration the error landed in — a namespace and a function — and
        // in both the error is inside the *body*, which is the case a rule based on the declarator alone would
        // have called clean.
        assert_eq!(unclean("namespace n { int bad = ; }\nint ok;\n"), ["n"]);
        assert_eq!(unclean("void f() { int a = ; }\nint ok;\n"), ["f"]);
    }

    #[test]
    fn only_the_innermost_declaration_is_touched() {
        // The error is inside `m`'s body: `m` is touched, and so is the class that contains it — but `a`, a
        // sibling member whose own declaration is untouched, is not. The wider rule, "any declaration whose range
        // contains the name", would mark `a` too; measured over the standard library's closure that rule marks
        // **2907** of 5388 declarations against **214** for this one, because a single error in a class body
        // condemns every member of it.
        assert_eq!(
            unclean("struct S { int a; void m() { int x = ; } };\nint ok;\n"),
            ["S", "m"]
        );
    }

    #[test]
    fn a_declaration_inside_a_function_body_is_local() {
        // The field `scope` cannot carry: `None` is both "at file scope", which every including file can name, and
        // "inside a function body", which nothing outside it can. Every kind of place a declaration can be written
        // is here, because the answer comes from the *scope chain* rather than from the declaration's shape.
        let (facts, _) = facts(
            "int global;\n\
             namespace ns { int in_a_namespace; }\n\
             struct C { int member; void method(); };\n\
             void f(int parameter) {\n\
               int local;\n\
               { int in_a_block; }\n\
               struct Local { int inner; };\n\
             }\n",
        );

        let local_of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("`{name}` is declared in the fixture"))
                .local
        };

        assert!(!local_of("global"), "file scope is reachable from outside");
        assert!(!local_of("in_a_namespace"), "a namespace member too");
        assert!(
            !local_of("member"),
            "a class body is not a function body, wherever the class is"
        );
        assert!(
            !local_of("method"),
            "and a member function's *declaration* is not its body"
        );
        assert!(
            local_of("parameter"),
            "a parameter is local to the function"
        );
        assert!(local_of("local"), "the case the field exists for");
        assert!(local_of("in_a_block"), "a nested block is still inside it");
        assert!(
            local_of("inner"),
            "and a class declared in there declares locals too"
        );
    }

    #[test]
    fn a_function_records_what_it_returns_and_nothing_else_does() {
        // The other half of `type_of`, and the two are deliberately exclusive: `make` is not a `Widget` — a member
        // access on the *name* has nothing to look in — while a call of it has one.
        let (facts, _) = facts(
            "Widget make();\n\
             Widget w;\n\
             struct C { Widget member(); };\n\
             static inline Widget decorated();\n\
             auto trailing() -> Widget;\n\
             auto deduced() { return Widget{}; }
        auto unclearable() { return make(); }\n\
             void plain();\n",
        );

        let of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("`{name}` is declared in the fixture"))
        };

        assert_eq!(of("make").returns.as_deref(), Some("Widget"));
        assert_eq!(
            of("make").type_of,
            None,
            "a function has no type of its own"
        );
        assert_eq!(
            of("w").type_of.as_deref(),
            Some("Widget"),
            "…and a variable returns nothing"
        );
        assert_eq!(of("w").returns, None);
        assert_eq!(
            of("decorated").returns.as_deref(),
            Some("Widget"),
            "declaration specifiers are stripped, as they are from `type_of`"
        );
        assert_eq!(
            of("trailing").returns.as_deref(),
            Some("Widget"),
            "a trailing return type wins — it is the one the file stated, and the specifiers say `auto`"
        );
        // **A deduced return the syntax can name is recorded.** `return Widget{…}` is a prvalue of `Widget`, and the
        // rule this assertion was written for — "a deduced `auto` is not a class anything can be looked up in" — is
        // true of a body whose returns this layer cannot type (`return f();`), not of one that names its type on the
        // line. Refusing the second is what left the hover without a return type and a caller without an input to
        // substitute into (reported, fig 5).
        assert_eq!(
            of("deduced").returns.as_deref(),
            Some("Widget"),
            "a construction in a `return` states the type it returns"
        );
        assert_eq!(
            of("unclearable").returns,
            None,
            "a return this layer cannot type is still refused rather than guessed"
        );
        assert_eq!(of("plain").returns.as_deref(), Some("void"));
    }
}

/// **A multi-byte character before an annotation does not panic.**
///
/// `the_annotation_before` finds the identifier by searching *backwards* for the first character that is not part
/// of it — and `rfind` answers a **byte** offset, so `offset + 1` lands inside the character when that character is
/// wider than one byte. Measured on a real project: *"start byte index 37747 is not a char boundary; it is inside
/// '×' (bytes 37746..37748)"*, raised inside the **indexing** thread while a summary was being built.
#[test]
fn an_annotation_after_a_multi_byte_character_is_read() {
    let written = "// 3×UCLASS()\nclass Foo {};";
    let at = written
        .find("class Foo")
        .expect("the declaration is written");
    let found = the_annotation_before(written, at);
    assert_eq!(
        found.map(|(name, _)| name),
        Some("UCLASS".to_string()),
        "the annotation is found past a two-byte character"
    );
}
