//! Semantic classification: **what each name in a file is**, as far as this analysis can tell.
//!
//! A semantic highlighter's question is asked once per identifier, so the answer has to be cheap — and the honest
//! cheap answer is "what does this name resolve to", which is the question this crate already answers everywhere
//! else. Nothing here is a new reading of the language:
//!
//! ```text
//! 1. the identifier *is* a declaration's name      the scope tree already bound it, with a kind  (`BindingKind`)
//! 2. the identifier is a name this file `#define`s  the preprocessor's directive list has it
//! 3. the identifier is *used* here and the file declares that spelling   the same binding, read by name
//! 4. otherwise                                      the index, asked **once per spelling** rather than per use
//! 5. and if nothing answers                         no classification — a name this analysis cannot place is
//!                                                   drawn as the client's default text, which is what it is
//! ```
//!
//! # Why the order is this, and why lookups are by name
//!
//! Measured (`examples/semantic_probe.rs`, the 109-file corpus): asking a *position* question per identifier —
//! the scope chain, then the index — costs **58 ms for a 1 038-identifier file and 160 ms for `sal.h`'s 10 987**,
//! because a failing lookup is a search: the index descends scopes looking for a name that is not there. Building
//! the file's own map of names once, and asking the index **once per distinct spelling**, brings the same file to
//! a few milliseconds — and the answers do not change: within one file the resolution of a spelling is a function
//! of the spelling and the file, not of the position.
//!
//! # What a name is *not* classified by
//!
//! Nothing here asks the parser whether a name stands in a type position. `Widget w;` and `Widget(1)` are a type
//! and a constructor call, and the tree shape differs — but the *kind* a highlighter wants is the one the
//! declaration has, and reading it from the declaration is how the two spellings agree. A name the analysis cannot
//! place at all gets no classification rather than a guess from its shape: a colour is a claim, and "this is a
//! type" is a claim this layer can only make when something declared it.

use std::collections::HashMap;

use cpp_parser::{CppTokenKind, SourceRange};

use crate::preprocess::directive::Directive;
use crate::sema::symbol::{Binding, BindingKind, ScopeKind};

use crate::{DeclFact, DeclKind, FileView, ProjectIndex, ScopeTree};

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
}

/// **What each name in this file is** — the classification a semantic highlighter draws colours from.
///
/// Every identifier in the file is considered, in source order; one that nothing can be said about is left out
/// rather than guessed at. The result is therefore a list of *claims*, and the caller draws only those.
///
/// # The four questions, in order
///
/// 1. **is this identifier a declaration's own name?** The scope tree's bindings carry a name range, so this is a
///    map lookup — and the binding's kind is finer than anything the index records: an enumerator is not a
///    variable, a constructor is not a function, and a *parameter* is distinguishable from a local because the
///    binding's scope is the function's parameter scope rather than a block inside the body.
/// 2. **is it a name this file `#define`s?** The directive list, which is the reading that includes a macro
///    defined in a branch nobody takes — an editor shows both branches, and a name the file defines is a macro to
///    the reader whatever the preprocessor would say about it here.
/// 3. **is it a use of a name this file declares?** The same map, read by spelling. Two declarations of one
///    spelling in different scopes (an overload, a shadowed local) make the spelling *ambiguous* for this purpose,
///    and an ambiguous spelling is classified by the **first** declaration the walk recorded — documented rather
///    than resolved, because resolving it is a position question and this pass exists to avoid those.
/// 4. **otherwise the index**, once per spelling: a name a header declares is a name the reader sees coloured.
///
/// # What the answer depends on, and what it does not
///
/// Only the file and the index — not the cursor, and not the client. A file whose includes have not been read yet
/// answers with fewer classifications rather than with different ones, so a caller publishing these may publish
/// them again when [`crate::Session::pending`] reaches zero; the ones already drawn stay true.
pub fn classified_names(index: &ProjectIndex, view: &FileView) -> Vec<Name> {
    let declared: HashMap<usize, &Binding> = bindings_by_offset(view);
    let by_spelling = bindings_by_spelling(view);
    let macros = macro_names(view);
    // **A parameter is a binding whose name stands in a parameter list.** The scope model cannot say so: a
    // function's body and its parameter scope are *one* scope (`ScopeKind::Function` documents why), so
    // `int f(int y) { int x; }` binds `y` and `x` in the same scope, in the same kind. The parameter list is the
    // tree's answer, read by the same function the scope builder and the inlay hints use — one reading of "what
    // are this function's parameters", three consumers.
    let parameters = parameter_offsets(view);

    let mut out: Vec<Name> = Vec::new();
    // The index is asked once per spelling — see the module documentation for what that is worth.
    let mut asked: HashMap<&str, Option<NameKind>> = HashMap::new();

    for token in view.tree.get_tokens() {
        if token.kind != CppTokenKind::Identifier {
            continue;
        }

        let range = token.range;
        let text = &view.source[range.start_offset..range.end_offset()];

        // 1. The declaration's own name.
        if let Some(binding) = declared.get(&range.start_offset)
            && let Some(kind) =
                kind_of_binding(binding, &view.scopes, parameters.contains(&range.start_offset))
        {
            out.push(Name {
                range,
                kind,
                declaration: true,
            });
            continue;
        }

        // 2. A macro this file defines. `#define` is the declaration; a use of the spelling is classified by the
        //    same pass, because a macro is a name like any other to a reader.
        if let Some(define) = macros.get(text) {
            out.push(Name {
                range,
                kind: NameKind::Macro,
                declaration: define.start_offset == range.start_offset,
            });
            continue;
        }

        // 3. A use of a name this file declares.
        if let Some(binding) = by_spelling.get(text)
            && let Some(kind) = kind_of_binding(
                binding,
                &view.scopes,
                parameters.contains(&binding.name_range.start_offset),
            )
        {
            out.push(Name {
                range,
                kind,
                declaration: false,
            });
            continue;
        }

        // 4. The index, asked once per spelling — the cache holds the *misses* too, which is what makes a file
        //    full of library names cheap rather than quadratic.
        let answer = asked.entry(text).or_insert_with(|| {
            index
                .definition(text, &view.path)
                .value()
                .and_then(|found| kind_of_fact(&found.fact))
        });
        if let Some(kind) = answer {
            out.push(Name {
                range,
                kind: *kind,
                declaration: false,
            });
        }

        // 5. Nothing. No token is emitted: the identifier keeps the client's own colour, which is the honest
        //    drawing of "this analysis cannot say".
    }

    out
}

/// The file's bindings, by the offset of the name each declares.
fn bindings_by_offset(view: &FileView) -> HashMap<usize, &Binding> {
    let mut map = HashMap::new();
    for scope in view.scopes.scopes() {
        for binding in &scope.bindings {
            map.insert(binding.name_range.start_offset, binding);
        }
    }

    map
}

/// The file's bindings, by the spelling they declare — the first one the walk recorded wins.
///
/// First rather than last, and by spelling rather than by position, because this pass exists to avoid position
/// questions: an overload set is one spelling with several declarations, and a reader who sees all of them
/// coloured as functions has been told the truth about every one of them. Two kinds under one spelling (a struct
/// `stat` and the function `stat`) resolve to whichever the walk reached first — documented, stable, and the same
/// answer on every request for an unchanged file.
fn bindings_by_spelling(view: &FileView) -> HashMap<String, &Binding> {
    let mut map: HashMap<String, &Binding> = HashMap::new();
    for scope in view.scopes.scopes() {
        for binding in &scope.bindings {
            map.entry(binding.name.text()).or_insert(binding);
        }
    }

    map
}

/// The names this file `#define`s, by spelling — with the range of each name as written.
///
/// From the **directives** rather than from the macro table in force, and the difference is deliberate: the table
/// answers "what is defined at this offset", which is a position question per identifier (measured at 155 ms for
/// `sal.h`), while a reader's question is "is this name a macro in this file at all". A macro defined in a branch
/// nobody takes is still a name the file defines and a reader sees.
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

/// The offsets of every **parameter's name** in the file.
///
/// Read from the tree rather than from the bindings, because a binding does not record how it was declared: the
/// scope model binds a parameter and a local in the same scope, in the same kind, and the two are the same thing
/// to every *lookup*. A highlighter is the one consumer that has to tell them apart — a reader expects a
/// parameter to look like a parameter — and the parameter list is where the difference is written.
fn parameter_offsets(view: &FileView) -> std::collections::HashSet<usize> {
    let mut offsets = std::collections::HashSet::new();

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

/// What a binding is, or `None` for a name a highlighter should not claim anything about.
fn kind_of_binding(binding: &Binding, scopes: &ScopeTree, is_a_parameter: bool) -> Option<NameKind> {
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
            if in_a_class(binding, scopes) {
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

/// Is this binding's scope inside a class?
fn in_a_class(binding: &Binding, scopes: &ScopeTree) -> bool {
    scopes
        .scope_chain(binding.scope)
        .into_iter()
        .any(|scope| {
            scopes
                .scope(scope)
                .is_some_and(|scope| scope.kind == ScopeKind::Class)
        })
}

/// What an indexed declaration is.
///
/// Coarser than a binding, and the two are not merged into one function because they are not the same question:
/// the index records what a summary can hold (a kind, a scope name, whether the declaration is local), while a
/// binding knows which *scope object* it was bound in. A parameter of a function in another file, for example,
/// arrives here as a local variable — the summary does not keep the distinction, and guessing it from the
/// spelling would be a guess.
fn kind_of_fact(fact: &DeclFact) -> Option<NameKind> {
    Some(match fact.kind {
        DeclKind::Type => NameKind::Type,
        DeclKind::Function => {
            if fact.scope.is_some() {
                NameKind::Method
            } else {
                NameKind::Function
            }
        }
        DeclKind::Variable => NameKind::Variable,
        DeclKind::Namespace => NameKind::Namespace,
        DeclKind::MacroLike => NameKind::Macro,
        DeclKind::Other => return None,
    })
}
