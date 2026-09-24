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
//! `docs/index-design.md`'s first invariant, and the reason is invalidation: a `#define` changes the meaning of
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

use std::collections::HashMap;

use crate::preprocess::directive::{Directive, DirectiveKind, SpannedDirective};
use crate::preprocess::FilePreprocessing;
use crate::sema::symbol::{Binding, BindingKind, ScopeId, ScopeTree};
use crate::summary::{DeclFact, DeclKind, FactGuard, SummaryGuards};
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
    // The kinds that have a type in this sense. A field and a parameter are `Variable` too — they are what a
    // member access is asked *from* — while a class and a function are not: a class *is* a type and a function
    // *returns* one, and those spellings come from a different part of the syntax.
    if binding.kind != BindingKind::Variable {
        return None;
    }

    // The declaration's own specifier sequence, found by **descending to where the binding is** and keeping the
    // last one passed on the way down.
    //
    // Not "the text before the name", which is what this started as and what a `Binding`'s range makes tempting:
    // a binding's range is the *declarator* (`w` in `Widget w;`) rather than the whole declaration, so the text in
    // front of it is empty and every variable's type came out as `None`. Descending from the root costs one walk
    // down the spine and asks the tree a structural question instead of reading text and hoping about geometry.
    let mut node = root.clone();
    let mut found = None;

    loop {
        if let Some(specifiers) = node
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq)
        {
            found = Some(specifiers.text().to_string());
        }

        match node
            .children_with_tokens()
            .find(|element| {
                element
                    .as_node()
                    .is_some_and(|child| holds(child, binding.range.start_offset))
            })
            .and_then(|element| element.into_node())
        {
            Some(child) => node = child,
            None => break,
        }
    }

    let spelling = strip_declaration_specifiers(&found?);

    (!spelling.is_empty()).then_some(spelling)
}

/// Is `offset` inside this node?
fn holds(node: &CppSyntaxNode, offset: usize) -> bool {
    let range = node.text_range();
    offset >= usize::from(range.start()) && offset < usize::from(range.end())
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
    if binding.kind != BindingKind::Function {
        return None;
    }

    // The walk `declared_type_of` makes, and for the same reason: a binding's range is the *declarator*, so the
    // tree is asked where the declaration is rather than the geometry of a range. Two things are collected on the
    // way: the last specifier sequence passed, and the last trailing return type.
    let mut node = root.clone();
    let mut specifiers = None;
    let mut trailing = None;

    loop {
        if let Some(found) = node
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq)
        {
            specifiers = Some(found.text().to_string());
        }
        if let Some(found) = node
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TrailingReturnType)
            && let Some(type_id) = found
                .children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TypeId)
        {
            trailing = Some(type_id.text().to_string());
        }

        match node
            .children_with_tokens()
            .find(|element| {
                element
                    .as_node()
                    .is_some_and(|child| holds(child, binding.range.start_offset))
            })
            .and_then(|element| element.into_node())
        {
            Some(child) => node = child,
            None => break,
        }
    }

    if let Some(trailing) = trailing {
        let spelling = trailing.trim().to_string();
        return (!spelling.is_empty()).then_some(spelling);
    }

    let spelling = strip_declaration_specifiers(&specifiers?);

    // A **deduced** return type is not a type this layer can name. `auto` and `decltype(auto)` are the two
    // spellings, and both would otherwise be looked up as class names — answering "no member `size` in `auto`"
    // where the honest answer is that the file never said.
    if spelling == "auto" || spelling == "decltype(auto)" {
        return None;
    }

    (!spelling.is_empty()).then_some(spelling)
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
    if binding.kind != BindingKind::Class {
        return Vec::new();
    }

    // The class definition the binding is inside — the last one passed on the way down to it, which is the walk
    // `declared_type_of` makes and for the same reason: a binding's range does not cover the construct that
    // declared it, so the *tree* is asked where the declaration is rather than the geometry of a range.
    let mut node = root.clone();
    let mut owner = None;

    loop {
        if matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::ClassDef | CppSyntaxKind::StructDef | CppSyntaxKind::EnumDef
        ) {
            owner = Some(node.clone());
        }

        match node
            .children_with_tokens()
            .find(|element| {
                element
                    .as_node()
                    .is_some_and(|child| holds(child, binding.range.start_offset))
            })
            .and_then(|element| element.into_node())
        {
            Some(child) => node = child,
            None => break,
        }
    }

    let Some(owner) = owner else {
        return Vec::new();
    };

    owner
        .children()
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::BaseSpecifier)
        .filter_map(|specifier| {
            specifier
                .children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr)
        })
        .map(|name| name.text().to_string().trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// The specifier keywords that can precede a type without being part of it.
///
/// Deliberately *not* in this list: `unsigned`, `signed`, `long`, `short`, `struct`, `class`, `enum`, `typename`.
/// The first four are the type itself; the last four name one, and stripping `struct` from `struct Widget` would
/// leave nothing to look up.
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
];

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
    binding: &Binding,
    scope: Option<String>,
    local: bool,
    declarations: &Declarations<'_>,
) -> Option<DeclFact> {
    // A binding with no identifier is a destructor, an operator, or a conversion function: real declarations,
    // but ones whose *name* is not a name a lookup can be keyed on. They are stored with an empty name and the
    // kind `Other` rather than dropped, because "there is a declaration here" is still the answer a
    // go-to-definition needs — the same decision `DeclKind::Other` documents.
    let name = binding
        .name
        .identifier_text()
        .unwrap_or_default()
        .to_string();

    Some(DeclFact {
        kind: DeclKind::from_binding_kind(binding.kind),
        name,
        scope,
        // Asked of the scope the binding was made in rather than of the declaration's shape: `bool` is the one
        // answer a *shape* cannot give, because `void f() { int x; }` and `void f() { }` differ by a declaration
        // that is not in a scope at all. See [`ScopeTree::declares_a_local`].
        local,
        type_of: declared_type_of(root, binding),
        returns: declared_returns_of(root, binding),
        bases: declared_bases_of(root, binding),
        range: binding.range,
        name_range: binding.name_range,
        // The one field that comes from the diagnostics rather than from the tree — see [`DeclFact::clean`] for
        // what the answer does and does not claim.
        clean: declarations.is_clean(binding.name_range, binding.range),
        // Filled in by `assign_guards`, which is the only place that knows where the directives are.
        guard: FactGuard::Unconditional,
    })
}

/// One declaration node of the file, and whether a diagnostic fell inside it.
struct Declared {
    range: cpp_parser::SourceRange,
    touched: bool,
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

        if !errors.is_empty() {
            for node in root.descendants() {
                let kind = CppSyntaxKind::from(node.kind());
                if !crate::scopes::declaring_kinds().contains(&kind) {
                    continue;
                }

                let range = cpp_parser::source_range(node.text_range());
                nodes.push(Declared {
                    range,
                    touched: errors.iter().any(|error| {
                        error.start_offset < range.end_offset()
                            && range.start_offset < error.end_offset()
                    }),
                });
            }

            nodes.sort_by_key(|declared| declared.range.length);
        }

        Declarations { nodes, errors }
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
            None => !self
                .errors
                .iter()
                .any(|error| overlaps(*error, own_range)),
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
    /// The tree, which is where a declared type's spelling comes from.
    root: &'a CppSyntaxNode,
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
            declarations: Declarations::of(root, errors),
            guards: SummaryGuards::default(),
            facts: Vec::new(),
        }
    }

    fn build(self) -> (Vec<DeclFact>, SummaryGuards) {
        // Destructured so that the walk below holds the three inputs and the two outputs as separate bindings:
        // the loop pushes into `facts` while reading `declarations`, which a `&mut self` method could not do.
        let DeclarationFacts {
            scopes,
            preprocessing,
            root,
            declarations,
            mut guards,
            mut facts,
        } = self;

        for (index, scope) in scopes.scopes().iter().enumerate() {
            // The prefix is what a declaration written *here* is qualified by, which is the scope's own name
            // for a namespace or a class and nothing at all for a function body or a block — see
            // [`ScopeTree::qualification_prefix_of`] for why the two questions have to be asked separately.
            let prefix = scopes.qualification_prefix_of(ScopeId(index));
            // …and the second thing the *scope* knows rather than the declaration: whether a name bound here can
            // be reached from outside the body it sits in. See [`DeclFact::local`].
            let local = scopes.declares_a_local(ScopeId(index));

            for binding in &scope.bindings {
                if let Some(fact) = fact_for(root, binding, prefix.clone(), local, &declarations) {
                    facts.push(fact);
                }
            }
        }

        // The sweep below needs both lists in offset order. Facts are sorted rather than built in order
        // because the scope tree's order is depth-first, which is not offset order — a nested class's members
        // are walked before the next top-level declaration.
        facts.sort_by_key(|fact| fact.range.start_offset);

        let mut targets: Vec<(&mut FactGuard, usize)> = facts
            .iter_mut()
            .map(|fact| (&mut fact.guard, fact.range.start_offset))
            .collect();
        assign_guards(&mut targets, preprocessing, &mut guards);

        (facts, guards)
    }
}

/// Give every fact the innermost conditional region it was written in.
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
    // Built into a local list rather than written straight into `guards`, and the reason is the read below: the
    // walk has to ask where each open region *starts*, which is a shared borrow of `guards` while a region is
    // still being appended. The list is small — one entry per conditional directive — so the extra allocation is
    // nothing next to having two mutable borrows of the same field.
    let mut regions: Vec<SourceRange> = Vec::new();
    let conditionals = conditional_regions(preprocessing, &mut regions);

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
}

/// Every directive that opens or closes a conditional, paired with the region it affects.
///
/// `observed_by` is the directive's **end**: a condition is not in force on the line it is written on, so
/// `#if A` must not be treated as open for a fact that starts before the directive finishes. That is the same
/// rule [`FilePreprocessing::guard_at`] applies, and it is the one that keeps a declaration on the `#if` line
/// itself outside the region it opens.
fn conditional_regions(
    preprocessing: &FilePreprocessing,
    regions_out: &mut Vec<SourceRange>,
) -> Vec<ConditionalDirective> {
    let mut regions: Vec<ConditionalDirective> = Vec::new();
    // Where each currently-open region's index is, so an `#else` or `#endif` can find the region it continues or
    // closes without re-walking the directive list.
    let mut open: Vec<usize> = Vec::new();

    for spanned in &preprocessing.directives {
        let kind = spanned.directive.kind();

        if kind.opens_a_condition() {
            let index = regions_out.len();
            // The region's range is the condition itself, completed when the matching `#endif` is reached — see
            // `extend_region_to`. A consumer asking "was this compiled" re-reads this span, which is why the span
            // has to be the condition and not the whole region: the body can be megabytes and the question is
            // about the condition.
            regions_out.push(condition_span(spanned));
            open.push(index);
            regions.push(ConditionalDirective {
                index,
                close: false,
                observed_by: spanned.range.end_offset(),
            });
        } else if kind.closes_a_condition() {
            let Some(index) = open.last().copied() else {
                // An `#endif` with no `#if`: malformed, and the recovery is to record nothing rather than to
                // attribute the rest of the file to a region that does not exist.
                continue;
            };

            // `#else` and `#elif` end the region they were in and start another with the same identity: the
            // region *is* the conditional, and a fact on either branch is in the same `#if`. So the region's span
            // is extended and the region stays open.
            let closes = kind == DirectiveKind::Endif;
            if closes {
                open.pop();
                extend_region_to(regions_out, index, spanned.range.end_offset());
            } else {
                extend_region_to(regions_out, index, spanned.range.end_offset());
                continue;
            }

            regions.push(ConditionalDirective {
                index,
                close: true,
                observed_by: spanned.range.end_offset(),
            });
        }
    }

    regions
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

/// A conditional directive, as the sweep needs it.
#[derive(Debug, Clone, Copy)]
struct ConditionalDirective {
    /// Which entry of [`SummaryGuards::regions`] this directive opens or closes.
    index: usize,
    /// Is this the `#endif` that ends the region, as opposed to the `#if` that began it?
    close: bool,
    /// The offset at which the directive is over, and therefore at which it takes effect.
    observed_by: usize,
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

    scopes.scopes().iter().enumerate().find_map(|(index, scope)| {
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

#[cfg(test)]
mod tests {
    use super::{build_facts, by_name};
    use crate::preprocess::preprocess;
    use crate::sema::scopes::build_scopes;
    use crate::summary::{DeclKind, FactGuard};
    use cpp_parser::{CppParser, ParserConfig};

    fn facts(source: &str) -> (Vec<crate::summary::DeclFact>, crate::summary::SummaryGuards) {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.get_errors(), [], "the input must parse cleanly");

        let root = tree.get_red_root();
        build_facts(&build_scopes(&root), &preprocess(&root), &root, &[])
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
            build_facts(&build_scopes(&root), &preprocess(&root), &root, &errors).0,
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
    fn unclean(source: &str) -> Vec<String> {        let (facts, errors) = facts_of_a_broken_file(source);
        assert!(!errors.is_empty(), "the fixture must have diagnostics");

        facts
            .iter()
            .filter(|fact| !fact.clean)
            .map(|fact| fact.name.clone())
            .collect()
    }

    #[test]
    fn a_file_that_parses_cleanly_is_clean() {        let (facts, _) = facts("struct S { int a; };\nint count;\nvoid f() { int local; }\n");

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
            facts.iter().map(|fact| fact.name.as_str()).collect::<Vec<_>>(),
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
    fn a_declaration_inside_a_function_body_is_local() {        // The field `scope` cannot carry: `None` is both "at file scope", which every including file can name, and
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
        assert!(local_of("parameter"), "a parameter is local to the function");
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
             auto deduced() { return Widget{}; }\n\
             void plain();\n",
        );

        let of = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.name == name)
                .unwrap_or_else(|| panic!("`{name}` is declared in the fixture"))
        };

        assert_eq!(of("make").returns.as_deref(), Some("Widget"));
        assert_eq!(of("make").type_of, None, "a function has no type of its own");
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
        assert_eq!(
            of("deduced").returns,
            None,
            "a deduced `auto` is not a class anything can be looked up in"
        );
        assert_eq!(of("plain").returns.as_deref(), Some("void"));
    }
}
