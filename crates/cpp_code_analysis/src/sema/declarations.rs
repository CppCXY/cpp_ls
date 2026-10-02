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

use std::collections::HashMap;

use crate::preprocess::directive::{Directive, DirectiveKind, SpannedDirective};
use crate::preprocess::FilePreprocessing;
use crate::sema::symbol::{Binding, BindingKind, ScopeId, ScopeTree};
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

fn declared_type_of_with(shapes: &DeclarationShapes, binding: &Binding) -> Option<String> {
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
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        if let Some(node) = &shape.specifiers {
            specifiers = Some(node.clone());
        }
        if declarator.is_none()
            && let Some(node) = &shape.declarator
            && reaches_the_name(node, binding.name_range)
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

    let found = crate::sema::types::type_of_declaration(
        &specifiers,
        declarator.as_ref(),
        binding.name_range,
    );

    let written = found.to_string();
    (!written.is_empty()).then_some(written)
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

        for node in root.descendants() {
            let at = cpp_parser::source_range(node.text_range());

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
                let default = match node
                    .parent()
                    .map(|owner| CppSyntaxKind::from(owner.kind()))
                {
                    Some(CppSyntaxKind::StructDef | CppSyntaxKind::UnionDef) => Access::Public,
                    // A `class` — and anything else this walk does not recognise, which is the conservative
                    // direction for a *member*: the private reading hides a name rather than offering one the
                    // reader cannot write.
                    _ => Access::Private,
                };
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
                && let Some(list) = node
                    .children()
                    .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TemplateParameterList)
                && let Some(owner) = node.parent()
            {
                for child in owner.children() {
                    if CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq {
                        templates.push((cpp_parser::source_range(child.text_range()), list.clone()));
                    }
                }
            }

            let read = children_a_question_reads(&node);
            if read.is_empty() && !declares_a_shape(kind) {
                continue;
            }

            let parent = chain.last().copied();
            shapes.push(Shape {
                at,
                kind,
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
        }
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
            let position =
                candidates.partition_point(|index| self.shapes[*index as usize].at.start_offset <= offset);
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
    let mut read = ShapeChildren {
        specifiers: None,
        trailing: None,
        declarator: None,
        type_id: None,
        bases: Vec::new(),
    };

    for child in node.children() {
        match CppSyntaxKind::from(child.kind()) {
            CppSyntaxKind::DeclSpecifierSeq if read.specifiers.is_none() => read.specifiers = Some(child),
            CppSyntaxKind::TrailingReturnType if read.trailing.is_none() => read.trailing = Some(child),
            CppSyntaxKind::Declarator if read.declarator.is_none() => read.declarator = Some(child),
            CppSyntaxKind::TypeId if read.type_id.is_none() => read.type_id = Some(child),
            // A base's *name*, not the whole specifier: taking the specifier's text would read the access keyword
            // as part of the name (`public B` is `B`).
            CppSyntaxKind::BaseSpecifier => read.bases.extend(
                child
                    .children()
                    .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr),
            ),
            _ => {}
        }
    }

    read
}

/// Is a node of this kind a shape on its own, with nothing to read from its children?
fn declares_a_shape(kind: CppSyntaxKind) -> bool {
    matches!(
        kind,
        CppSyntaxKind::UsingDecl
            | CppSyntaxKind::TypedefDecl
            | CppSyntaxKind::ClassDef
            | CppSyntaxKind::StructDef
            | CppSyntaxKind::EnumDef
    )
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

    // No declarator: `typedef struct { … } ;` has no name to bind either, so this cannot be reached with a fact
    // to build — but returning the specifiers is the honest answer if it ever is.
    let Some(declarator) = declarator else {
        let spelling = strip_declaration_specifiers(&specifiers.text().to_string());
        return (!spelling.is_empty()).then_some(spelling);
    };

    let spelling = format!(
        "{} {}",
        strip_declaration_specifiers(&specifiers.text().to_string()),
        without(&declarator.text().to_string(), declarator, binding.name_range)
    );
    let spelling = spelling.split_whitespace().collect::<Vec<_>>().join(" ");

    (!spelling.is_empty()).then_some(spelling)
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
    declared_returns_of_with(&DeclarationShapes::of(root), binding)
}

/// [`declared_returns_of`] for a caller that has the file's shapes already.
fn declared_returns_of_with(shapes: &DeclarationShapes, binding: &Binding) -> Option<String> {
    if binding.kind != BindingKind::Function {
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
            && let Some(type_id) = node
                .children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TypeId)
        {
            let spelling = type_id.text().to_string().trim().to_string();
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
        .map(|declarator| before(&declarator.text().to_string(), declarator, binding.name_range))
        .filter(|operators| operators.chars().all(|c| c == '*' || c == '&' || c.is_whitespace()))
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
    let spelling = if specifiers.text().to_string().trim().is_empty() {
        String::new()
    } else {
        crate::sema::types::read_specifiers(&specifiers).to_string()
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

    let body = token
        .parent_ancestors()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ClassBody)?;

    let default = match body.parent().map(|owner| CppSyntaxKind::from(owner.kind())) {
        Some(CppSyntaxKind::StructDef | CppSyntaxKind::UnionDef) => Access::Public,
        _ => Access::Private,
    };

    let mut level = default;
    for child in body.children() {
        // Only the labels **before** the name are in force: a `private:` written below it does not change what this
        // declaration is, which is the same rule the file's own text follows.
        if usize::from(child.text_range().start()) >= offset {
            break;
        }

        level = match CppSyntaxKind::from(child.kind()) {
            CppSyntaxKind::PublicAccess => Access::Public,
            CppSyntaxKind::ProtectedAccess => Access::Protected,
            CppSyntaxKind::PrivateAccess => Access::Private,
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

    token.parent_ancestors().any(|node| begins_an_exported_region(&node))
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

    let starts_with_export = node
        .children_with_tokens()
        .find_map(|element| element.into_token())
        .is_some_and(|token| {
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

    let declarator = token
        .parent_ancestors()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declarator)?;

    crate::inlay::parameter_list_text(&declarator)
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
fn declared_template_parameters_with(shapes: &DeclarationShapes, binding: &Binding) -> Vec<String> {
    if binding.kind != BindingKind::Class {
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
fn declared_bases_of_with(shapes: &DeclarationShapes, binding: &Binding) -> Vec<String> {
    if binding.kind != BindingKind::Class {
        return Vec::new();
    }

    // The class-like definition the binding is inside — "the last one passed on the way down", which is the walk
    // [`declared_type_of`] makes and for the same reason: a binding's range does not cover the construct that
    // declared it, so the *tree* is asked where the declaration is rather than the geometry of a range.
    let mut owner = None;
    shapes.on_the_path(the_offset_to_descend_by(binding), |shape| {
        if matches!(
            shape.kind,
            CppSyntaxKind::ClassDef | CppSyntaxKind::StructDef | CppSyntaxKind::EnumDef
        ) {
            owner = Some(shape.bases.clone());
        }
    });

    let Some(bases) = owner else {
        return Vec::new();
    };

    bases
        .iter()
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
    declarations: &Declarations<'_>,
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

    // **The four type questions, timed apart.** Together they are 98% of the index's sweep on a real project
    // (138 files of MSVC's standard library), and each one is a different traversal of the same tree — so which of
    // them dominates is the difference between four different fixes. See `crate::stages`.
    let type_of = {
        let _timer = crate::stages::StageTimer::new(crate::stages::Stage::TypeOf);
        declared_type_of_with(shapes, binding)
    }
    .or_else(|| {
        let _timer = crate::stages::StageTimer::new(crate::stages::Stage::Alias);
        declared_alias_target_with(shapes, binding)
    });
    let returns = {
        let _timer = crate::stages::StageTimer::new(crate::stages::Stage::Returns);
        declared_returns_of_with(shapes, binding)
    };
    let bases = {
        let _timer = crate::stages::StageTimer::new(crate::stages::Stage::Bases);
        declared_bases_of_with(shapes, binding)
    };
    // The class template's parameter names, for the members whose types are written with them. Answered from the
    // shapes like the four questions above it, and on a unit read it is the reason they are all asked of a table
    // rather than of the tree — see [`declared_template_parameters_of`].
    let parameters = {
        let _timer = crate::stages::StageTimer::new(crate::stages::Stage::TemplateParameters);
        declared_template_parameters_with(shapes, binding)
    };
    // **Who may name it.** Recorded on the shape the binding's name sits in, which the pass that built this table
    // already knows — see [`Shape::access`]. `None` for a name no class body contains.
    let access = shapes.access_at(the_offset_to_descend_by(binding));
    // **…and whether an importer may.** The other half of "who may name it", for a file that declares a module:
    // see [`Shape::exported`] and [`crate::ProjectIndex::visible_files`], which is where it is applied.
    let exported = shapes.exported_at(the_offset_to_descend_by(binding));
    // **What the function was declared with**, which is the one thing a reader picking a name out of a hundred
    // needs and a fact did not carry: `format` and `format_to` are two rows of a completion that used to read
    // `string (…)` and `_OutputIt (…)`. Read from the tree at the name's own offset — see
    // [`parameter_list_at`] for why the declarator is found that way rather than through the shapes.
    let parameter_list = match binding.kind {
        BindingKind::Function
        | BindingKind::Constructor
        | BindingKind::Destructor
        | BindingKind::ConversionFunction
        | BindingKind::OperatorFunction
        | BindingKind::LiteralOperator => parameter_list_at(root, binding.name_range.start_offset),
        _ => None,
    };

    Some(DeclFact {
        kind: DeclKind::from_binding_kind(binding.kind),
        name,
        scope,
        // Asked of the scope the binding was made in rather than of the declaration's shape: `bool` is the one
        // answer a *shape* cannot give, because `void f() { int x; }` and `void f() { }` differ by a declaration
        // that is not in a scope at all. See [`ScopeTree::declares_a_local`].
        local,
        type_of,
        returns,
        bases,
        parameters,
        parameter_list,
        access,
        exported,
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
            shapes: {
                let _shapes = crate::stages::StageTimer::new(crate::stages::Stage::Shapes);
                DeclarationShapes::of(root)
            },
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

        for (index, scope) in scopes.scopes().iter().enumerate() {
            // The prefix is what a declaration written *here* is qualified by, which is the scope's own name
            // for a namespace or a class and nothing at all for a function body or a block — see
            // [`ScopeTree::qualification_prefix_of`] for why the two questions have to be asked separately.
            let prefix = scopes.qualification_prefix_of(ScopeId(index));
            // …and the second thing the *scope* knows rather than the declaration: whether a name bound here can
            // be reached from outside the body it sits in. See [`DeclFact::local`].
            let local = scopes.declares_a_local(ScopeId(index));

            for binding in &scope.bindings {
                if let Some(fact) = fact_for(root, &shapes, binding, prefix.clone(), local, &declarations) {
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
        DirectiveKind::Ifndef => BranchCondition::NotDefined(branch.condition.clone().unwrap_or_default()),
        DirectiveKind::Ifdef => BranchCondition::Defined(branch.condition.clone().unwrap_or_default()),
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
                open_branch(&mut found.conditionals, index, &spanned.directive, &spanned.range);
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
    let Some((defined, rest)) = rest.split_first().filter(|(token, _)| token.text() == "defined") else {
        return BranchCondition::Other;
    };
    let _ = defined;

    let parenthesised = rest
        .first()
        .is_some_and(|token| token.kind == cpp_parser::CppTokenKind::LeftParen);
    let rest = if parenthesised { &rest[1..] } else { rest };

    let Some((name, rest)) = rest.split_first().filter(|(token, _)| token.is_identifier()) else {
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
                    || settles.get(&(parent as usize, name)).copied().unwrap_or(false)
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
        assert!(!exported("plain"), "and a declaration with no `export` above it");
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
            !names.contains(&"size_type") || names.iter().filter(|name| **name == "size_type").count() == 1,
            "and no member is named after the type: {names:?}"
        );

        // The same member with an inline **body**, which is how `basic_string::size` is really written, and with a
        // directive block before it — the shape the class body's member loop had to learn to walk through.
        let defined = "struct S {\n#if FEATURE\n  int a;\n#endif\n  _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR\n  size_type\n  size() const noexcept\n  { return 0; }\n};\n";
        let (defined_facts, _) = facts(defined);

        let names: Vec<&str> = defined_facts.iter().map(|fact| fact.name.as_str()).collect();
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
        build_facts(&build_scopes(&root, &crate::NoMacroBodies), &preprocess(source, tree.get_tokens()), &root, &[])
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
            build_facts(&build_scopes(&root, &crate::NoMacroBodies), &preprocess(source, tree.get_tokens()), &root, &errors).0,
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



