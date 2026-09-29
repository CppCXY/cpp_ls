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
    /// A function declared in a class — a member function, a constructor, a destructor, an operator.
    Method,
    Variable,
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
    /// The one modifier this layer can honestly set, and it is the one a reader benefits from most: clients draw a
    /// declaration in a different weight, which is how "where does this come from" is answered at a glance.
    pub declaration: bool,
    /// Which layer answered, and by which rule — see [`Provenance`].
    pub provenance: Provenance,
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
    let _timer = crate::stages::StageTimer::new(crate::stages::Stage::Classify);

    let declared = declarations_by_offset(view);
    let parameters = parameter_offsets(view);
    let macros = macro_names(view);
    let defined = definitions_of(view);
    let members = member_offsets(view);
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
    // [`member_offsets`].
    let mut held: HashMap<&str, Option<NameKind>> = HashMap::new();
    let mut asked: HashMap<String, Option<NameKind>> = HashMap::new();
    let mut out: Vec<Name> = Vec::new();

    for token in view.tree.get_tokens() {
        if token.kind != CppTokenKind::Identifier {
            continue;
        }

        let range = token.range;
        let text = &view.source[range.start_offset..range.end_offset()];
        let at = range.start_offset;

        // 1. The declaration's own name. One hash lookup, and it is asked of every identifier in the file.
        if let Some(binding) = declared.get(&at)
            && let Some(kind) = kind_of_binding(binding, view, parameters.contains(&binding.name_range.start_offset))
        {
            out.push(Name {
                range,
                kind,
                declaration: true,
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
                provenance: Provenance::Macro,
            });
            continue;
        }

        // 3. A declaration this file's own scopes can name. **Asked only for a spelling this file declares at all**:
        //    a scope walk that cannot succeed is the expensive way to learn nothing, and most of what a real
        //    header's identifiers are is names declared somewhere else entirely.
        //
        //    A **member access** skips this step and the filter both: `w.size` is resolved through the type of `w`,
        //    which is not a scope, so what this file declares under that spelling says nothing about it.
        let a_member_access = members.contains(&at);
        let declared_here = defined.contains(text);
        if declared_here && !a_member_access {
            let resolved = *held.entry(text).or_insert_with(|| match held_at(view, at) {
                Held::InThisFile(binding) => kind_of_binding(
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
                provenance: Provenance::Found,
            });
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
fn member_offsets(view: &FileView) -> HashSet<usize> {
    fn writes_the_operator(node: &cpp_parser::CppSyntaxNode) -> bool {
        node.children_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| matches!(token.text(), "." | "->"))
    }

    view.root
        .descendants()
        .filter(|node| {
            matches!(
                cpp_parser::CppSyntaxKind::from(node.kind()),
                cpp_parser::CppSyntaxKind::MemberExpr
                    | cpp_parser::CppSyntaxKind::ArrowExpr
                    | cpp_parser::CppSyntaxKind::IndexExpr
            )
        })
        .filter(writes_the_operator)
        .filter_map(|node| crate::sema::resolve::member_access_of(&node))
        .filter(|access| access.member_range.length > 0)
        .map(|access| access.member_range.start_offset)
        .collect()
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
    scopes
        .scope_chain(scope)
        .into_iter()
        .any(|id| scopes.scope(id).is_some_and(|scope| scope.kind == ScopeKind::Class))
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
        if cpp_parser::CppSyntaxKind::from(list.kind()) != cpp_parser::CppSyntaxKind::ParameterList {
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

    for node in view.root.descendants().filter(is_a_name_node).filter(writes_a_qualifier) {
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

/// What a binding is, or `None` for a name a highlighter should not claim anything about.
fn kind_of_binding(binding: &Binding, view: &FileView, is_a_parameter: bool) -> Option<NameKind> {
    Some(match binding.kind {
        BindingKind::Namespace => NameKind::Namespace,
        BindingKind::Class | BindingKind::Enum | BindingKind::Alias | BindingKind::Typedef => {
            NameKind::Type
        }
        BindingKind::TemplateParameter => NameKind::TypeParameter,
        BindingKind::Enumerator => NameKind::EnumMember,
        // **Member or free** is a fact about the scope the binding lives in, which the model already records: a
        // function declared inside a class is a method, and a constructor is one whatever it is called.
        BindingKind::Function
        | BindingKind::Constructor
        | BindingKind::Destructor
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
        DeclKind::Variable => NameKind::Variable,
        DeclKind::Namespace => NameKind::Namespace,
        DeclKind::MacroLike => NameKind::Macro,
        DeclKind::Other => return None,
    })
}









