//! Semantic classification: **what each name in a file is**, as far as this analysis can tell.
//!
//! A semantic highlighter's question is asked once per identifier in a file, and the honest cheap answer is "what
//! does this name resolve to" — the question this crate already answers everywhere else, through the scope tree for
//! a name written here and through the index for one a header declares. Nothing here is a new reading of the
//! language. What it is, is the **bulk form** of that question:
//!
//! ```text
//! for every identifier token, in source order:
//!   1. is it the name a declaration gives itself?      the scope tree's bindings carry the name's own span, so
//!                                                      this is one hash lookup  →  `declaration: true`
//!   2. is it a `#define` this file writes?             the directive list, which is also the only layer that can
//!                                                      answer for a macro — a macro is not a declaration
//!   3. otherwise, which declaration does it refer to?  `sema::resolve::definition_at` — this file's own scopes —
//!                                                      and then the index, asked **once per distinct spelling**
//!   4. and if nothing answers                        **no** classification: a colour is a claim, and this layer
//!                                                      only makes claims it can support
//! ```
//!
//! # Why the reference question, and not a spelling map
//!
//! The first version of this module answered step 3 by **spelling**: it built a map from "what this file declares",
//! keyed by the declared name, and classified every use of that spelling as if it named that declaration. It is
//! cheap — one map, no lookup — and it is wrong in the ways a reader notices first:
//!
//! ```text
//! struct Widget { int size; };        the use of `size` in `scale` refers to the **parameter**, and the use of
//! int size(int value);                `size` in `area` is a member of `Widget`. A map keyed on the spelling
//! int scale(Widget& w, int size) {    cannot tell either of them apart from the file-scope function that
//!     return w.size * size;           happens to share the spelling, so it drew all three as `Function`.
//! }
//! ```
//!
//! The correction is not a better guess at the same question — it is asking the question the answer is a fact
//! about, which this crate already knows how to ask. Both misclassifications are pinned as tests
//! (`tests/semantic.rs`), and the vocabulary that falls out of it ([`Provenance`]) is what the probe prints.
//!
//! # Where each answer comes from, said out loud
//!
//! Each [`Name`] carries its [`Provenance`], because "the scope tree says so" and "the index says so" are answers
//! with different strengths: the first is a binding with a kind, the second a summary fact whose kind is coarser (a
//! parameter of a function in another file arrives as a local variable — the summary does not keep the
//! distinction). A consumer that wants to draw only what is certain can filter on it, a probe can print it, and a
//! reader of a bug report can see which layer answered.
//!
//! # Why the file's own answer is never second-guessed
//!
//! [`definition_at`] implements C++ ordinary lookup over one file's scopes — the shadowing rule, the enclosing
//! scopes, `using namespace` targets. Whatever it finds **is** what the name means in this file, so the index is not
//! consulted on top of it: an index keyed on bare names cannot see a scope, and asking it would replace a known
//! answer with a coarser one. The index is asked exactly when the file's scopes have nothing to say, which is the
//! case it exists for: a name a header declares.
//!
//! A name the file declares somewhere but **cannot see from here** is the same rule read the other way, and it is
//! why the step below does not fall through to the index on a miss: what the file declares under that spelling is
//! either out of scope at this position or a member of a class, and an index keyed on bare names would answer with
//! some *other* declaration that happens to share the spelling.
//!
//! # What a name is *not* classified by
//!
//! Nothing here asks the parser whether a name stands in a type position. `Widget w;` and `Widget(1)` are a type
//! and a constructor call, and the tree shape differs — but the *kind* a highlighter wants is the one the
//! **declaration** has, and reading it from the declaration is how the two spellings agree. A name the analysis
//! cannot place at all gets no classification rather than a guess from its shape.
//!
//! # What the answer depends on, and what it does not
//!
//! The file's text, its scope tree, and the index — not the cursor, and not the client. A file whose includes have
//! not been read yet answers with **fewer** classifications rather than with different ones: a smaller index
//! resolves fewer spellings, and every name this file declares itself is answered the same way whatever the index
//! holds.
//!
//! # What it costs, and where that was measured
//!
//! `examples/semantic_probe.rs` is the instrument, and it prints the time and the distribution of the answers by
//! [`Provenance`]. Measured on the MinGW standard-library closure (356 files, the 45 largest classified, 19 797
//! identifiers, release): **35 ms for 5 591 classifications — 1.8 µs per identifier**. Three findings from building
//! it are worth keeping, because each one was two orders of magnitude away from where it looked:
//!
//! ```text
//! · a tree descent per spelling                  600 ms of a 630 ms pass. `qualified_name_at` walks down to the
//!                                                offset, which is the right price for a cursor and the wrong one
//!                                                for every name in a file — see `qualified_names`
//! · a member access rejected by reading it        `(a && b)` parses as the same node kind `w.size` does, so a
//!                                                header of 447 `#if defined(…)` conditions paid a subtree walk per
//!                                                condition to learn there is no member access in it
//! · a scope walk for a name no scope binds        one `HashSet` of the spellings this file declares is the filter
//! ```

use std::collections::{HashMap, HashSet};
use std::path::Path;

use cpp_parser::{CppTokenKind, SourceRange};

use crate::index::project::IndexedKind;
use crate::preprocess::directive::Directive;
use crate::sema::resolve::definition_at;
use crate::sema::symbol::{Binding, BindingKind, Known, ScopeId, ScopeKind, ScopeTree};

use crate::{DeclKind, FileView, ProjectIndex};

/// What a name is, in as many kinds as this analysis can honestly tell apart.
///
/// The set is chosen against what a client can draw (the protocol's own token types) rather than against the
/// model: a caller maps these onto its legend, and a kind nobody can draw is a kind that should not be here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameKind {
    Namespace,
    Type,
    /// A template parameter: a type *name* that is not a type.
    TypeParameter,
    /// An enumerator: `Red` in `enum { Red }`.
    EnumMember,
    /// A free function, or a function this layer cannot see the owner of.
    Function,
    /// **A variable whose type is a function pointer** — `binary_op operation = add;`.
    ///
    /// A kind of its own rather than `Function`, and the difference is what a theme wants: the name *is* a variable
    /// (`int (*)(int, int)` is the type, not the declaration), and it is *called* like a function — so a client can
    /// draw it either way, and this is the only place that says which two things it is between. A client that cannot
    /// draw the custom name is sent `Function`, which is the reading of every use of it.
    FunctionPointer,
    /// A function declared in a class — a member function, a constructor, a destructor, an operator.
    Method,
    Variable,
    /// **A variable template** — `template <class T> constexpr bool is_trivially_assignable_v = …`.
    ///
    /// A kind of its own because a reader sees one: the bare name is not a value (`is_trivially_assignable_v<A, B>`
    /// is), and a theme that draws it exactly like a `constexpr bool` says it can be used as one. A *use* that
    /// resolves to the template is drawn with it too — the name refers to the template either way.
    VariableTemplate,
    /// A function parameter, which the scope model can tell from a local (see [`classified_names`]).
    Parameter,
    /// A name this file `#define`s.
    Macro,
}

/// Where a classification came from — which layer answered, and therefore how strong the answer is.
///
/// Not decoration: these are different kinds of evidence, and a consumer that has to decide whether to draw a name
/// at all needs to be able to tell them apart. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// **This identifier is the name a declaration gives itself** — the strongest answer there is, and the only
    /// one that sets [`Name::declaration`] without a heuristic.
    DeclaredHere,
    /// The name resolves to a declaration in **this file**, found by the scope tree: a local, a parameter, a
    /// member of a class written here, a file-scope function. The binding carries the exact kind, which is what
    /// makes a parameter distinguishable from a local.
    Held,
    /// The name resolves to a declaration the **index** holds — in this file or in a header this file includes.
    /// A summary's kind is coarser than a binding's, so this is the weaker of the two resolved answers.
    Found,
    /// The name is a `#define` written in this file. A use of it is classified too, because a macro is a name like
    /// any other to a reader — and the one this file writes is the one in front of them.
    Macro,
}

/// One name in a file, with what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    /// The name's own span — the identifier token, not the declaration around it.
    pub range: SourceRange,
    pub kind: NameKind,
    /// Is this where the name is declared, rather than a use of it?
    ///
    /// The one modifier this layer could set at first, and it is the one a reader benefits from most: clients draw a
    /// declaration in a different weight, which is how "where does this come from" is answered at a glance.
    pub declaration: bool,
    /// **What else is true of the declaration**, where the file's own text says so. See [`NameModifier`].
    pub modifiers: Vec<NameModifier>,
    /// Which layer answered, and by which rule — see [`Provenance`].
    pub provenance: Provenance,
}

/// **A property of a name that is not its kind** — what a client draws as a *modifier* beside the colour.
///
/// The protocol has a fixed set of these and a client declares which it can draw, so this is chosen against that set
/// rather than against the language: `readonly` for a `const` object, `static` for one with internal linkage or no
/// instance, `deprecated` for a declaration that says so.
///
/// # Why only the file's own declarations have one
///
/// Because a modifier is a fact about a **declaration**, and the only declarations this layer can read are the ones
/// in the file in hand: the specifiers are tokens of a tree that is already parsed. A declaration reached through
/// the index carries a kind, a scope and a type — and none of those is "is this object `const`" — so a use of a
/// header's `const` variable is drawn without the modifier rather than with a guess. That is the direction this whole
/// module fails in: a colour missing is a colour the client's own theme supplies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameModifier {
    /// A `const` object: the declaration's own specifiers say so.
    ReadOnly,
    /// `static`, at file scope or in a class.
    Static,
    /// A declaration marked deprecated — `[[deprecated]]` or the MSVC `[[deprecated]]` spelling of it.
    Deprecated,
}

/// **What each name in this file is** — the classification a semantic highlighter draws colours from.
///
/// Every identifier in the file is considered, in source order; one that nothing can be said about is left out
/// rather than guessed at. The result is therefore a list of *claims*, and the caller draws only those.
///
/// # The questions, in order
///
/// 1. **is this identifier a declaration's own name?** The scope tree's bindings carry a name range, so this is a
///    map lookup — and the binding's kind is finer than anything the index records: an enumerator is not a
///    variable, a constructor is not a function, and a *parameter* is distinguishable from a local.
/// 2. **is it a `#define` this file writes?** A macro is not a declaration in the C++ tree at all, so nothing in
///    the scope tree can answer for it — and where a spelling is both, the preprocessor's reading is the one a
///    reader is looking at.
/// 3. **otherwise, which declaration does it refer to?** [`definition_at`] answers for the position, within this
///    file: the scope chain from the offset, the enclosing scopes, and `using namespace` targets. It is the same
///    answer "go to definition" gives, so a name that jumps somewhere is a name that is coloured — and a shadowed
///    local is coloured as the local rather than as the file-scope declaration sharing its spelling.
/// 4. **otherwise the index**, asked once per **spelling** — and asked with the name as written, so `std::size_t`
///    is a question about that name rather than about `size_t`. The cache holds the misses too, which is what makes
///    a file full of library names cheap rather than quadratic.
///
/// A member access (`w.size`) is deliberately **not** resolved by step 3: the member belongs to the type of `w`,
/// the index is what knows a class's members, and claiming one from the scope chain is the spelling mistake in
/// another costume. Such a name reaches step 4, where the question is the bare name the file wrote — and where an
/// answer that several unrelated declarations share comes back `Ambiguous` and gets **no** colour, which is this
/// layer's way of refusing to guess.
pub fn classified_names(index: &ProjectIndex, view: &FileView) -> Vec<Name> {
    // Timed because this runs on the interactive path — once per request for the file being edited — and because
    // "how much does a colour cost" is a question the probe in `examples/semantic_probe.rs` answers from here
    // rather than from a second implementation of the pass.

    let declared = declarations_by_offset(view);
    let parameters = parameter_offsets(view);
    let macros = macro_names(view);
    let defined = definitions_of(view);
    let members = member_kinds(index, view);
    let qualified = qualified_names(view);

    // **Both layers are asked once per spelling.** Neither question depends on *where* in the file the name is
    // written in a way that a spelling cannot carry: a macro is in force from its `#define` to the end of the file,
    // and a name that is not a declaration's own name means the same thing at every use of it — `definition_at`
    // resolves through the scopes containing the offset, and two uses of one spelling are resolved among the scopes
    // they are both inside of. Caching is what makes a header with a thousand identifiers a thousand lookups rather
    // than a thousand scope walks, and the **misses are cached too**, which matters more than the hits here: what a
    // real header is full of is names this file does not declare, and the answer for each of those is a whole
    // visibility walk.
    //
    // The one shape that cannot be cached by spelling is a member access, which is a question about a type — see
    // [`member_kinds`], which answers it and is asked once per access rather than once per spelling.
    let mut held: HashMap<&str, Option<NameKind>> = HashMap::new();
    // **The file's macro environment, built at most once** — see step 4b and
    // [`ProjectIndex::macro_names_in_force`]. It used to be a map from spelling to answer, which meant one walk of the
    // include graph per spelling; on `codeformat.h` that was 526 ms for 521 tokens.
    let included_macros: std::cell::OnceCell<std::collections::HashSet<String>> =
        std::cell::OnceCell::new();
    let mut asked: HashMap<String, Option<NameKind>> = HashMap::new();
    let mut out: Vec<Name> = Vec::new();

    // **The declaration shape table, built at most once for the whole file.** Reading a declaration's `const`-ness
    // is a question about the specifiers in front of it, and the table that answers it is a walk of the file — so
    // asking it per name is a walk per name. Measured on the standard library's `<algorithm>` (7 966 identifiers,
    // 1 638 of them declarations): **11.3 s**, and the table was rebuilt 1 638 times. Now built on the first name
    // that needs it (a file whose colouring reaches no value declaration never builds one) and shared by the rest.
    let shapes: std::cell::OnceCell<crate::sema::declarations::DeclarationShapes> =
        std::cell::OnceCell::new();

    for token in view.tree.get_tokens() {
        if token.kind != CppTokenKind::Identifier {
            continue;
        }

        let range = token.range;
        // `get_token_text` rather than a slice: a token's range is **the text's** when the tree is the text's own,
        // and this pass is also run against a rendering whose ranges need not fit it — where a slice panics and the
        // accessor answers `None`.
        let Some(text) = view.get_token_text(range) else {
            continue;
        };
        let at = range.start_offset;

        // 1. The declaration's own name. One hash lookup, and it is asked of every identifier in the file.
        if let Some(binding) = declared.get(&at)
            && let Some(kind) = kind_of_binding(
                index,
                binding,
                view,
                parameters.contains(&binding.name_range.start_offset),
            )
        {
            out.push(Name {
                range,
                kind,
                declaration: true,
                modifiers: modifiers_of(view, at, kind, &shapes),
                provenance: Provenance::DeclaredHere,
            });
            continue;
        }

        // 2. A macro this file defines. A name that is both a `#define` and something the scope tree can name is
        //    **the macro** to a reader — the preprocessor sees text, and the declaration sharing the spelling is a
        //    different thing that happens to be spelled the same way.
        if let Some(define) = macros.get(text) {
            out.push(Name {
                range,
                kind: NameKind::Macro,
                declaration: define.start_offset == at,
                modifiers: Vec::new(),
                provenance: Provenance::Macro,
            });
            continue;
        }

        // 3. A declaration this file's own scopes can name. **Asked only for a spelling this file declares at all**:
        //    a scope walk that cannot succeed is the expensive way to learn nothing, and most of what a real
        //    header's identifiers are is names declared somewhere else entirely.
        //
        //    A **member access** is answered before this step and instead of it: `w.size` is resolved through the
        //    type of `w`, which is not a scope, so what this file declares under that spelling says nothing about it.
        //    See [`member_kinds`], which is where that answer comes from — and which is why a member of a
        //    standard-library class is coloured now rather than falling through to a bare-name query that the whole
        //    project answers `Ambiguous`.
        if let Some(kind) = members.get(&at) {
            out.push(Name {
                range,
                kind: *kind,
                declaration: false,
                modifiers: modifiers_of(view, at, *kind, &shapes),
                provenance: Provenance::Found,
            });
            continue;
        }

        let declared_here = defined.contains(text);
        if declared_here {
            let resolved = *held.entry(text).or_insert_with(|| match held_at(view, at) {
                Held::InThisFile(binding) => kind_of_binding(
                    index,
                    &binding,
                    view,
                    parameters.contains(&binding.name_range.start_offset),
                ),
                Held::NotHere => None,
            });

            if let Some(kind) = resolved {
                out.push(Name {
                    range,
                    kind,
                    declaration: false,
                    // **A use carries no modifiers.** They are properties of a *declaration*, and this position is not
                    // one — the shape table would answer with whatever declaration happens to enclose the use, which
                    // is how `x` in `int x = n;` would take `n`'s `const` if `n` were declared in the same statement.
                    modifiers: Vec::new(),
                    provenance: Provenance::Held,
                });
                continue;
            }

            // **The index is not asked about a name this file declares but cannot see here.** Either the
            // declaration the scope chain found is in a class or a namespace, in which case an index keyed on bare
            // names is answering about some *other* declaration that shares the spelling — or the resolution failed
            // for a reason the index would fail for too (a local of a function this position is not inside, a
            // member of a class the file has). The index is for the case it exists for: a name nothing here
            // declares.
            continue;
        }

        // 4. The index, asked once per spelling — with the name as written, so a qualified one stays qualified.
        let written = qualified
            .get(&at)
            .cloned()
            .unwrap_or_else(|| text.to_string());
        let answer = asked.entry(written.clone()).or_insert_with(|| {
            index
                .kind_of(&written, &view.path)
                .value()
                .and_then(kind_of_index)
        });
        if let Some(kind) = answer {
            out.push(Name {
                range,
                kind: *kind,
                declaration: false,
                // A use, and the index's answer — neither is a declaration in this tree, so there are no specifiers
                // to read. See the `modifiers_of` note: the file's own declarations are the only ones whose
                // `const`-ness this layer can see.
                modifiers: Vec::new(),
                provenance: Provenance::Found,
            });
        }

        // 4b. **A macro an `#include` brought in.** Step 2 answers for the `#define`s this file writes, and a file that
        //     uses `<gtest/gtest.h>`'s `TEST(…)` or `LuaIdentify.h`'s `lisxdigit(…)` declares neither — measured from an
        //     editor: both were drawn as ordinary identifiers while hovering them showed the `#define` perfectly, which
        //     is the analysis saying it knows what the name is and the highlighter never asking.
        //
        //     Asked **after** the two resolution steps rather than before them, because the answer costs a walk of the
        //     include graph and the common case in a header is a name the index can answer — while a macro that
        //     *shadows* a resolvable name is rare, and keeps the colour of the name it shadows.
        //
        //     **One walk for the whole file**, not one per spelling. Measured on this repository's own
        //     `codeformat.h`: 521 tokens took **526 ms**, because a header mentions a few hundred names that nothing
        //     in it declares and each of those walked the include graph. The environment is a property of the *file*,
        //     and [`ProjectIndex::macro_names_in_force`] is where the single walk lives.
        let included = included_macros.get_or_init(|| index.macro_names_in_force(&view.path));
        let is_a_macro = included.contains(text);
        if is_a_macro {
            out.push(Name {
                range,
                kind: NameKind::Macro,
                declaration: false,
                modifiers: Vec::new(),
                provenance: Provenance::Macro,
            });
            continue;
        }

        // 5. Nothing. No token is emitted: the identifier keeps the client's own colour, which is the honest
        //    drawing of "this analysis cannot say".
    }

    out
}

/// The file's **own** declarations, by the offset of the name each gives itself.
///
/// A binding's `name_range` is the identifier the declaration introduces — the same span a rename edits — which is
/// exactly the offset an identifier token starts at. So "is this token a declaration's own name" is one hash
/// lookup per token, and it is asked for every identifier in the file.
fn declarations_by_offset(view: &FileView) -> HashMap<usize, &Binding> {
    let mut map = HashMap::new();
    for scope in view.scopes.scopes() {
        for binding in &scope.bindings {
            map.insert(binding.name_range.start_offset, binding);
        }
    }

    map
}

/// What the name written at `offset` refers to, **within this file**.
enum Held {
    /// The scope chain resolved it to a declaration this file writes — the answer, and the index is not asked.
    InThisFile(Binding),
    /// This file's scopes have nothing to say about the name at this position. Whether that means "ask the index"
    /// is the caller's question, and it is not the same one: a spelling the file declares *somewhere* but cannot
    /// see *here* is not a name the index can help with — see [`classified_names`].
    NotHere,
}

/// Every spelling this file **declares as an ordinary identifier**: a binding's own name, from every scope in the
/// table.
///
/// Not the same question as [`declarations_by_offset`], which is keyed by *where* a name is declared; this is keyed
/// by the name, and asked about a *use*. It exists as a filter rather than as an answer: a scope walk that cannot
/// succeed is the expensive way to learn nothing, and most of what a header's identifiers are is names declared
/// somewhere else entirely — in the standard library, or nowhere this file can see.
///
/// Ordinary identifiers only, which is why the raw spelling is read rather than [`crate::Name::text`]: a
/// constructor, a conversion operator and a literal are *found* by their own rules (a destructor is not found by
/// ordinary lookup at all), so a `~`/`operator`-shaped spelling matched against raw source text would be a filter
/// that lets nothing through and costs a lookup to learn it.
fn definitions_of(view: &FileView) -> HashSet<&str> {
    view.scopes
        .scopes()
        .iter()
        .flat_map(|scope| scope.bindings.iter())
        .filter_map(|binding| binding.name.identifier_text())
        .collect()
}

/// The offsets at which the file writes a **member access** — the `size` of `w.size`.
///
/// Keyed by *offset* and not by spelling, which is the whole point: `size` is also the name of a parameter in the
/// fixture that pins this, and a set of spellings would make every `size` in the file a member access. The same
/// spelling in two places can be two different questions, so the position is part of the question.
///
/// An access whose member is not written yet (`w.`) contributes nothing, which is right — there is no name to
/// classify.
///
/// # Why the operator is looked for before the member is read
///
/// The parser produces an [`IndexExpr`](cpp_parser::CppSyntaxKind::IndexExpr) for `w.size` **and** for `(a && b)` —
/// one node kind with a `.` where brackets would be, which is why [`member_access_of`] decides by the operator
/// rather than by the kind. So a file full of `#if defined(A) && defined(B)` is a file full of nodes this walk has
/// to *reject*, and rejecting one costs whatever `member_access_of` costs: it collects every identifier below the
/// operator it is looking for. On a header of 447 nested conditions that was the entire cost of the pass — a
/// subtree walk per condition to learn that there is no member access in it.
///
/// The operator among the node's **direct children** answers the same question for the price of one child list,
/// because an operator is a direct child of the expression it belongs to.
///
/// [`member_access_of`]: crate::sema::resolve::member_access_of
/// **What else is true of the declaration a name is**, from the file's own tokens.
///
/// The specifiers are the tokens between the start of a declaration and its name, and they are read the way every
/// other spelling in this project is: by text, from a tree that is already parsed. `constexpr` counts as `const`
/// because it is one — an object `constexpr int n` cannot be written through either — and `const` is looked for as a
/// **word**, so `constexpr` and `const_iterator` are not mistaken for it.
///
/// `None` for a name whose declaration this layer cannot see: a use of a name from a header, whose specifiers are not
/// in any tree this pass holds. See [`NameModifier`] for why that is the right answer rather than a guess.
/// `shapes` is the **file's** shape table, built once by the caller — see [`classified_names`]. It is a
/// [`OnceCell`](std::cell::OnceCell) because a file whose colouring reaches no value declaration should not pay for
/// one, and a file with two thousand of them must not pay for two thousand.
fn modifiers_of(
    view: &FileView,
    name_at: usize,
    kind: NameKind,
    shapes: &std::cell::OnceCell<crate::sema::declarations::DeclarationShapes>,
) -> Vec<NameModifier> {
    // **Only what can carry these modifiers at all.** `readonly` and `static` are properties of an *object* and of a
    // function — not of a type, a namespace or an enumerator — and asking the shape table for them anyway is what
    // produced a `struct Widget` marked `readonly static`, measured: the specifiers a shape records are not always
    // the ones a reader would point at. A modifier is a visible claim about the code, and the cheap way not to make
    // a wrong one is not to make it where it cannot be true.
    if !matches!(
        kind,
        NameKind::Variable
            | NameKind::FunctionPointer
            | NameKind::Parameter
            | NameKind::Method
            | NameKind::Function
    ) {
        return Vec::new();
    }

    // The declaration's own specifiers, in the shape table — the same source `access_at` reads, and for the same
    // reason: it is one entry per declaration rather than a walk of the tree.
    let shapes =
        shapes.get_or_init(|| crate::sema::declarations::DeclarationShapes::of(&view.root));
    let Some(specifiers) = shapes.specifiers_at(name_at) else {
        return Vec::new();
    };

    let written = specifiers.text().to_string();
    let words: Vec<&str> = written
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .collect();

    let mut out = Vec::new();
    if words
        .iter()
        .any(|word| matches!(*word, "const" | "constexpr" | "consteval"))
    {
        out.push(NameModifier::ReadOnly);
    }
    if words.contains(&"static") {
        out.push(NameModifier::Static);
    }
    if written.contains("deprecated") {
        out.push(NameModifier::Deprecated);
    }
    out
}

/// **What each member access in this file names**, by the offset of the member.
///
/// # Why a scope tree cannot answer this, and what the first version settled for
///
/// `w.size` is resolved through the **type of `w`**, which is not a scope: what this file declares under the
/// spelling `size` says nothing about it. The first version collected the member offsets and let them fall through
/// to the index by their **bare name** — and that is where the feature stopped working, because a bare member name
/// is exactly the spelling the whole project shares: measured on a real file, `v.push_back` and `t.size()` got **no
/// colour at all** (the index answers `Ambiguous` and this layer refuses to guess) while `sux.print()` did, because
/// `print` happens to be unique in that project. A reader saw every standard-library member go grey and concluded
/// the highlighter was crude — which it was, in the one place members are the whole point.
///
/// The answer is the one the note above has always named: ask the **type**. [`member_across_files`] is the query
/// that does it — the same one a jump to a member's declaration uses, so a member that can be jumped to is a member
/// that is coloured — and its answer carries the declaration's kind, which is what a colour is chosen from.
fn member_kinds(index: &ProjectIndex, view: &FileView) -> HashMap<usize, NameKind> {
    fn writes_the_operator(node: &cpp_parser::CppSyntaxNode) -> bool {
        node.children_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| matches!(token.text(), "." | "->"))
    }

    let mut kinds = HashMap::new();

    for node in view.root.descendants().filter(|node| {
        matches!(
            cpp_parser::CppSyntaxKind::from(node.kind()),
            cpp_parser::CppSyntaxKind::MemberExpr
                | cpp_parser::CppSyntaxKind::ArrowExpr
                | cpp_parser::CppSyntaxKind::IndexExpr
        )
    }) {
        if !writes_the_operator(&node) {
            continue;
        }

        let Some(access) = crate::sema::resolve::member_access_of(&node) else {
            continue;
        };
        if access.member_range.length == 0 {
            continue;
        }

        // A file the declaring class is in is not opened here: the class's own members come from the index, and the
        // object's type comes from this file. `None` for the view means a query that needs another file's text
        // answers nothing, which is this layer's ordinary way of declining rather than guessing — and it costs one
        // `Option` rather than a parse of a header per member.
        let found = crate::index::project::member_across_files(
            index,
            &mut |_: &Path| None,
            &view.scopes,
            &view.root,
            &view.path,
            access.member_range.start_offset,
        );

        let Known::Yes(found) = found else {
            continue;
        };

        // The same mapping the index answer goes through — see [`kind_of_index`] — so `push_back` is a method and
        // not a function, which is the difference between the colour a member gets and the one a free function does.
        let kind = match found.fact.kind {
            DeclKind::Function if found.fact.scope.is_some() => NameKind::Method,
            DeclKind::Function => NameKind::Function,
            DeclKind::Variable => NameKind::Variable,
            DeclKind::Type => NameKind::Type,
            DeclKind::Namespace => NameKind::Namespace,
            DeclKind::MacroLike => NameKind::Macro,
            DeclKind::Other => continue,
        };

        kinds.insert(access.member_range.start_offset, kind);
    }

    kinds
}

/// The declaration the name at `offset` refers to, within this file — see [`Held`].
///
/// The caller has already excluded the two shapes that are not this question: a declaration's own name (a map
/// lookup, and the only one that sets [`Name::declaration`]) and a member access ([`member_offsets`], and the note
/// there for why a scope tree cannot answer one). What is left is ordinary lookup, which is [`definition_at`]'s own
/// question, so this is a call rather than a rule.
fn held_at(view: &FileView, offset: usize) -> Held {
    match definition_at(&view.scopes, &view.root, offset) {
        Known::Yes(binding) => Held::InThisFile(binding),
        _ => Held::NotHere,
    }
}

/// Is this scope inside a class?
fn in_a_class(scope: ScopeId, scopes: &ScopeTree) -> bool {
    scopes.scope_chain(scope).into_iter().any(|id| {
        scopes
            .scope(id)
            .is_some_and(|scope| scope.kind == ScopeKind::Class)
    })
}

/// The offsets of every **parameter's name** in the file.
///
/// Read from the tree rather than from the bindings, because a binding does not record how it was declared: the
/// scope model binds a parameter and a local in the same scope, in the same kind, and the two are the same thing
/// to every *lookup*. A highlighter is the one consumer that has to tell them apart — a reader expects a
/// parameter to look like a parameter — and the parameter list is where the difference is written.
///
/// Keyed by the span the name was declared with, so one set answers both questions this module asks: "is this
/// declaration a parameter" (the token's own offset is in it) and "does this use name a parameter" (the binding's
/// `name_range` is).
fn parameter_offsets(view: &FileView) -> HashSet<usize> {
    let mut offsets = HashSet::new();

    for list in view.root.descendants() {
        if cpp_parser::CppSyntaxKind::from(list.kind()) != cpp_parser::CppSyntaxKind::ParameterList
        {
            continue;
        }

        for (_, declared) in crate::sema::scopes::parameters_of(&list) {
            if let Some((_, name_range)) = declared {
                offsets.insert(name_range.start_offset);
            }
        }
    }

    offsets
}

/// The names this file `#define`s, by spelling — with the range of each name as written.
///
/// From the **directives** rather than from the macro table in force, and the difference is deliberate: the table
/// answers "what is defined at this offset", which is a position question per identifier, while a reader's question
/// is "is this name a macro in this file at all". A macro defined in a branch nobody takes is still a name the file
/// defines and a reader sees.
fn macro_names(view: &FileView) -> HashMap<String, SourceRange> {
    let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
    let mut map = HashMap::new();

    for spanned in &preprocessing.directives {
        if let Directive::Define(define) = &spanned.directive
            && let Some(definition) = &define.macro_def
        {
            map.entry(definition.name.to_string())
                .or_insert(definition.name_range);
        }
    }

    map
}

/// The spellings the file writes **with a qualifier**, by the offset each identifier starts at: `std::size_t` at
/// the offset of `size_t`, and `ns::Widget<int>` at the offset of `Widget`.
///
/// Asked because an index keyed on bare names cannot tell `a::Widget` from `b::Widget`, and a file that writes the
/// qualifier is telling the analysis which one it means.
///
/// # Why it is a map built in one walk rather than a query per name
///
/// [`qualified_name_at`](crate::sema::resolve::qualified_name_at) answers this for a cursor, and it does so by
/// **descending the tree** from the root to the offset — which is the right price for one cursor and the wrong one
/// for a pass that asks about every spelling in a file. Measured on `bits/version.h` (3 669 identifiers, 606 `::`
/// written in `#if defined(__glibcxx_want_…)` conditions): calling it per spelling was **600 ms of a 630 ms
/// pass** — 95% of the work, spent descending to names that are not qualified at all.
///
/// The walk here looks for the `::` among a name node's **direct children** first, which is what tells a qualified
/// spelling from a bare one, and descends no further for the names that have none. A bare name needs no entry: its
/// spelling *is* the token's own text, which is what the caller already has.
fn qualified_names(view: &FileView) -> HashMap<usize, String> {
    fn is_a_name_node(node: &cpp_parser::CppSyntaxNode) -> bool {
        matches!(
            cpp_parser::CppSyntaxKind::from(node.kind()),
            cpp_parser::CppSyntaxKind::NameExpr | cpp_parser::CppSyntaxKind::IdentifierExpr
        )
    }

    fn writes_a_qualifier(node: &cpp_parser::CppSyntaxNode) -> bool {
        node.children_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| token.text() == "::")
    }

    let mut map = HashMap::new();

    for node in view
        .root
        .descendants()
        .filter(is_a_name_node)
        .filter(writes_a_qualifier)
    {
        // One walk of the node's tokens, as `qualified_name_at` does it: the identifiers in order, joined with
        // `::`, stopping the spelling at the identifier the offset is on — which is what makes a cursor on a
        // *qualifier* mean the qualifier (`a::b::c` asked about `b` is `a::b`).
        let mut written = String::new();
        let mut global = false;

        for element in node.children_with_tokens() {
            let Some(token) = element.into_token() else {
                continue;
            };

            match cpp_parser::CppTokenKind::from(token.kind()) {
                cpp_parser::CppTokenKind::Identifier => {
                    if !written.is_empty() {
                        written.push_str("::");
                    }
                    written.push_str(token.text());

                    let at = usize::from(token.text_range().start());
                    map.insert(at, spell(global, &written));
                }
                // A leading `::` is part of the spelling and means the global name space — see
                // `qualified_name_at`, whose convention this is.
                cpp_parser::CppTokenKind::Scope if written.is_empty() => global = true,
                _ => {}
            }
        }
    }

    map
}

/// The name as written, with the leading `::` of a global name put back.
fn spell(global: bool, written: &str) -> String {
    if global {
        format!("::{written}")
    } else {
        written.to_string()
    }
}

/// **Does this spelling name a function pointer — directly, or through one typedef?**
///
/// `int (*)(int, int)` is one, and so is `binary_op` while the file writes
/// `typedef int (*binary_op)(int, int);`. The second spelling is the one a reader sees in
/// `binary_op operation = add;`, and a declaration's recorded type is the alias's name rather than the signature, so
/// the alias is followed in the index.
///
/// The chain is bounded because a typedef of a typedef is legal and a cycle is not: four steps is past any spelling a
/// project writes, and a spelling that returns to itself is refused rather than followed again.
fn denotes_a_function_pointer(index: &ProjectIndex, declaring: &Path, written: &str) -> bool {
    let mut spelling = written.trim().to_string();
    for _ in 0..4 {
        // **The signature is the answer.** `int (*)(int, int)` and `void (*)(void)` both carry `(*`, and no other
        // type spelling does — a pointer to a function is written exactly that way.
        if spelling.contains("(*") {
            return true;
        }
        let Known::Yes(found) = index.definition(&spelling, declaring) else {
            return false;
        };
        if found.fact.kind != DeclKind::Type {
            return false;
        }
        let Some(target) = found.fact.type_of.as_deref() else {
            return false;
        };
        let target = target.to_string();
        if target == spelling {
            return false;
        }
        spelling = target;
    }
    false
}

/// What a binding is, or `None` for a name a highlighter should not claim anything about.
fn kind_of_binding(
    index: &ProjectIndex,
    binding: &Binding,
    view: &FileView,
    is_a_parameter: bool,
) -> Option<NameKind> {
    Some(match binding.kind {
        BindingKind::Namespace => NameKind::Namespace,
        BindingKind::Class | BindingKind::Enum | BindingKind::Alias | BindingKind::Typedef => {
            NameKind::Type
        }
        BindingKind::TemplateParameter => NameKind::TypeParameter,
        BindingKind::Enumerator => NameKind::EnumMember,
        // **A constructor and a destructor are the class's own name, so they are drawn as the class.**
        //
        // Reported from an editor: `ExpectedVoid()` came out as `method` — `entity.name.function.member` in VS Code's
        // own theme — while the class name above it was `type`. A constructor's name *is* the class's name
        // (`[class.ctor]`: a constructor has no name of its own), and a theme colours the two the same way for the
        // same reason: a reader scanning `class ExpectedVoid { ExpectedVoid(); }` is looking at one name written
        // twice, not at a type and a function.
        //
        // It also matters beyond the declaration. A constructor's name shares its spelling with the class, so a
        // *use* of that spelling — `ExpectedVoid(const ExpectedVoid& other)`, where the parameter's type is the
        // class — resolves through the scope chain to whichever binding is nearer, and grouping the kinds keeps the
        // answer a type either way round. Measured before this: every `ExpectedVoid` in the file was `method`.
        BindingKind::Constructor | BindingKind::Destructor => NameKind::Type,
        // **Member or free** is a fact about the scope the binding lives in, which the model already records: a
        // function declared inside a class is a method.
        BindingKind::Function
        | BindingKind::ConversionFunction
        | BindingKind::OperatorFunction
        | BindingKind::LiteralOperator => {
            if in_a_class(binding.scope, &view.scopes) {
                NameKind::Method
            } else {
                NameKind::Function
            }
        }
        BindingKind::Variable => {
            if is_a_parameter {
                NameKind::Parameter
            } else if !crate::sema::declarations::declared_template_parameters_of(
                &view.root, binding,
            )
            .is_empty()
            {
                // **`template <class T> constexpr bool v = …;`** — the declaration introduces a family of values,
                // while the scope model records a variable because that is all a binding can say. The template head
                // is in the tree, and this is what asks it.
                NameKind::VariableTemplate
            } else if crate::sema::declarations::declared_type_of(&view.root, binding)
                .is_some_and(|written| denotes_a_function_pointer(index, &view.path, &written))
            {
                // **A function pointer is drawn as a function.** `binary_op operation = add;` declares a *variable* —
                // the fact says so and hover says so — but a reader scanning `operation(1, 2)` reads a call, and a
                // theme that colours functions is where that call belongs. The type is what decides, so an ordinary
                // variable keeps `variable` and only a callable one moves.
                NameKind::FunctionPointer
            } else {
                NameKind::Variable
            }
        }
        // A `using` declaration, a label, a name this layer could not read: nothing a colour would improve on.
        BindingKind::UsingDeclaration
        | BindingKind::UsingDirective
        | BindingKind::Label
        | BindingKind::Other => return None,
    })
}

/// What an indexed declaration of this shape is, for a colour.
///
/// Coarser than a binding, and the two are not merged into one function because they are not the same question: the
/// index records what a summary can hold (a kind, a scope name, whether the declaration is local), while a binding
/// knows which *scope object* it was bound in. A parameter of a function in another file, for example, arrives here
/// as a local variable — the summary does not keep the distinction, and guessing it from the spelling would be a
/// guess.
///
/// **A scoped function is a method**, and a namespace is the price of that: a summary records the scope's
/// *spelling* and not whether it is a class, so `std::to_string` arrives looking exactly like a member. The
/// alternative is to draw every library function as a free one, which is wrong about all of them; this is wrong
/// about the ones in a namespace, and only in the modifier a client draws differently.
fn kind_of_index(found: IndexedKind) -> Option<NameKind> {
    Some(match found.kind {
        DeclKind::Type => NameKind::Type,
        DeclKind::Function if found.scope.is_some() => NameKind::Method,
        DeclKind::Function => NameKind::Function,
        // **An enumerator is not a variable to a reader.** The index has one kind for both — and says which is
        // which on [`IndexedKind::enumerator`] — while a theme draws an enumerator with its enumeration. Drawn as a
        // plain variable, `Color::Red` read as a global constant that happened to live inside a type.
        DeclKind::Variable if found.enumerator => NameKind::EnumMember,
        // A variable **template**'s declaration: a family of values rather than one — the distinction the buffer
        // path reads from its tree and this path reads from the fact's recorded template parameters.
        DeclKind::Variable if found.template => NameKind::VariableTemplate,
        DeclKind::Variable => NameKind::Variable,
        DeclKind::Namespace => NameKind::Namespace,
        DeclKind::MacroLike => NameKind::Macro,
        DeclKind::Other => return None,
    })
}
