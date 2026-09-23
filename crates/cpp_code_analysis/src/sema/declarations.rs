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
pub fn build_facts(
    scopes: &ScopeTree,
    preprocessing: &FilePreprocessing,
    root: &CppSyntaxNode,
) -> (Vec<DeclFact>, SummaryGuards) {
    let file = DeclarationFacts::new(scopes, preprocessing, root);
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
fn fact_for(root: &CppSyntaxNode, binding: &Binding, scope: Option<String>) -> Option<DeclFact> {
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
        type_of: declared_type_of(root, binding),
        bases: declared_bases_of(root, binding),
        range: binding.range,
        name_range: binding.name_range,
        // Filled in by `assign_guards`, which is the only place that knows where the directives are.
        guard: FactGuard::Unconditional,
    })
}

/// The walk's state: the regions opened so far, and the facts collected.
struct DeclarationFacts<'a> {
    scopes: &'a ScopeTree,
    preprocessing: &'a FilePreprocessing,
    /// The tree, which is where a declared type's spelling comes from.
    root: &'a CppSyntaxNode,
    guards: SummaryGuards,
    /// Every fact, in the order the scopes hold them — sorted by offset once, before the guard sweep.
    facts: Vec<DeclFact>,
}

impl<'a> DeclarationFacts<'a> {
    fn new(
        scopes: &'a ScopeTree,
        preprocessing: &'a FilePreprocessing,
        root: &'a CppSyntaxNode,
    ) -> Self {
        DeclarationFacts {
            scopes,
            preprocessing,
            root,
            guards: SummaryGuards::default(),
            facts: Vec::new(),
        }
    }

    fn build(mut self) -> (Vec<DeclFact>, SummaryGuards) {
        // Moved out up front so that the walk below borrows three fields of `self` separately rather than all of
        // it — the loop pushes into `self.facts` while reading the other two.
        let preprocessing = self.preprocessing;
        let scopes = self.scopes;
        let root = self.root;

        for (index, scope) in scopes.scopes().iter().enumerate() {
            // The prefix is what a declaration written *here* is qualified by, which is the scope's own name
            // for a namespace or a class and nothing at all for a function body or a block — see
            // [`ScopeTree::qualification_prefix_of`] for why the two questions have to be asked separately.
            let prefix = scopes.qualification_prefix_of(ScopeId(index));

            for binding in &scope.bindings {
                if let Some(fact) = fact_for(root, binding, prefix.clone()) {
                    self.facts.push(fact);
                }
            }
        }

        // The sweep below needs both lists in offset order. Facts are sorted rather than built in order
        // because the scope tree's order is depth-first, which is not offset order — a nested class's members
        // are walked before the next top-level declaration.
        self.facts.sort_by_key(|fact| fact.range.start_offset);

        let mut guards = std::mem::take(&mut self.guards);
        let targets: Vec<(&mut FactGuard, usize)> = self
            .facts
            .iter_mut()
            .map(|fact| (&mut fact.guard, fact.range.start_offset))
            .collect();
        let mut targets = targets;
        assign_guards(&mut targets, preprocessing, &mut guards);

        (self.facts, guards)
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
        build_facts(&build_scopes(&root), &preprocess(&root), &root)
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
}
