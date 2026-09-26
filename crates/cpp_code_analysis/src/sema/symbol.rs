//! The symbol model: names, scopes, bindings, and **what is not known**.
//!
//! This is the vocabulary the analysis layer speaks. It deliberately contains no lookup and no traversal —
//! those are the next step, and their correctness depends on getting these shapes right first.
//!
//! # Why knowledge is three-valued here
//!
//! A compiler resolves C++ completely or fails. An editor cannot: it analyses code that is half-written,
//! code behind conditions it cannot evaluate, and code whose meaning depends on macros that were expanded
//! somewhere it did not look. The temptation is to answer anyway, and the cost is a wrong answer that looks
//! exactly like a right one.
//!
//! So a resolved question here answers [`Known<T>`], which has a third value:
//!
//! ```text
//! Known::Yes(symbol)          -> this name means that declaration
//! Known::No                   -> this name is definitely not declared here
//! Known::Unknown(reason)      -> no answer is available, and here is why
//! ```
//!
//! `No` and `Unknown` are the distinction that matters, and collapsing them is the mistake this module
//! exists to prevent. "This name is not declared" makes a consumer grey out code or report an error; "I
//! could not tell" makes it stay quiet. The existing layers already work this way — [`crate::guard::Visibility`]
//! separates `Inactive` from `Unknown`, and [`crate::modules::ImportOutcome`] separates "no such module" from
//! "I cannot find it" — and [`UnknownReason`] is what lets a consumer explain itself instead of going silent.
//!
//! # Why the reason is a closed enum
//!
//! Because each variant has a different fix, and a consumer that cannot tell them apart can only ever say
//! "sorry". `MacroExpansion` means expand more or accept the gap; `UnresolvedInclude` means configure the
//! include path; `DependentName` means the answer depends on a template argument that is not known yet.
//! Making the set closed means adding a new source of uncertainty is a compile error at every match, which
//! is how the gaps stay visible instead of accumulating silently.
//!
//! # Why a name is not a string
//!
//! `operator+`, `~Widget`, `operator int`, and `Widget` are four different things that a `String` cannot tell
//! apart, and the difference is semantic rather than cosmetic: a constructor and a destructor are **not**
//! found by ordinary unqualified lookup, so a model that stored them as the class name would make
//! `~Widget` resolve to the class and `Widget w;` resolve to the constructor. [`NameKind`] keeps them apart,
//! and [`Name::is_ordinary`] is the predicate a lookup keys on.
//!
//! The spelling is kept alongside the kind because a consumer has to print it, and re-deriving `operator+`
//! from a token stream is a second implementation of a rule the parser already applied.

use std::collections::HashSet;

use cpp_parser::SourceRange;

use crate::paths::FileId;
use crate::summary::MacroScopeReading;

/// A question's answer, with "no answer" as a first-class value.
///
/// See the module documentation for why the third value exists. `Unknown` carries a reason rather than a
/// unit, because a consumer that can say *why* it does not know is useful and one that can only say "no" is
/// not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Known<T> {
    Yes(T),
    No,
    /// No answer is available. Boxed so that a reason does not widen every `Known<SymbolId>` in the program —
    /// the common case is `Yes`, and it should not pay for the rare one.
    Unknown(UnknownReason),
}

impl<T> Known<T> {
    /// Did the question get an answer?
    pub fn is_known(&self) -> bool {
        matches!(self, Known::Yes(_))
    }

    /// Is this a definite no, as opposed to an absence of an answer?
    ///
    /// The predicate a consumer wants before it reports anything: only a definite no justifies "this name is
    /// not declared".
    pub fn is_definitely_absent(&self) -> bool {
        matches!(self, Known::No)
    }

    /// The answer, if there is one.
    pub fn value(self) -> Option<T> {
        match self {
            Known::Yes(value) => Some(value),
            _ => None,
        }
    }

    /// The answer by reference, if there is one.
    pub fn value_ref(&self) -> Option<&T> {
        match self {
            Known::Yes(value) => Some(value),
            _ => None,
        }
    }

    /// Why there is no answer, if that is the state.
    pub fn reason(&self) -> Option<&UnknownReason> {
        match self {
            Known::Unknown(reason) => Some(reason),
            _ => None,
        }
    }

    /// Transform the answer, leaving the other two states alone.
    ///
    /// The other two states are why this is not `Option::map`: a caller chaining transformations must not
    /// lose the difference between "no" and "unknown" on the way through.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Known<U> {
        match self {
            Known::Yes(value) => Known::Yes(f(value)),
            Known::No => Known::No,
            Known::Unknown(reason) => Known::Unknown(reason),
        }
    }

    /// The reason to give when a transformation cannot produce an answer.
    ///
    /// `None` when the input was a definite `No`, which stays a definite `No`: a question that already had
    /// an answer is not made uncertain by failing to refine it.
    pub fn then_unknown(self, reason: UnknownReason) -> Known<T> {
        match self {
            Known::No => Known::No,
            _ => Known::Unknown(reason),
        }
    }
}

/// Why a question could not be answered.
///
/// Every variant is a source of uncertainty that C++ genuinely has, and each one has a different remedy. The
/// set is closed on purpose: a new source of doubt has to be added here, and every `match` in the program
/// then fails to compile until it is handled — which is how a gap stays visible instead of becoming a
/// silent `No`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// The answer is inside a macro expansion this analysis did not perform.
    ///
    /// The name is the macro's, and it is kept so the message can say which one. The expansion's range is
    /// *not* kept: [`SourceRange`] is not `Hash` or `Ord`, and duplicating it here to save a lookup would
    /// make this type unusable as a map key for no gain — a consumer with a range already has it.
    MacroExpansion(Box<str>),
    /// The answer is inside a conditional region that could not be evaluated.
    ///
    /// Distinct from "the region is not compiled": a branch that is definitely dead has an answer (`No`), and
    /// only an undecidable one is unknown. See [`crate::guard::Visibility::Unknown`].
    ConditionalCompilation,
    /// The answer is in a header that was not found.
    UnresolvedInclude(Box<str>),
    /// The answer is in a module that is not part of the analysis.
    ///
    /// The everyday case is `import std;`, which is normally a prebuilt BMI with no source in the project —
    /// so "the module was not found" says nothing about whether the name exists.
    UnresolvedModule(Box<str>),
    /// The name depends on a template argument that is not known.
    ///
    /// `T::value_type` where `T` is a template parameter: the answer exists only at instantiation, and
    /// guessing a particular instantiation would produce an answer that is wrong for every other one.
    DependentName,
    /// The declaration is inside a template that has not been instantiated.
    ///
    /// Distinct from [`UnknownReason::DependentName`]: here the *declaration* is in a template body, so its
    /// contents are not known until instantiation, rather than a name being dependent on a parameter already
    /// in scope.
    UnexpandedTemplate,
    /// The name cannot be read from the syntax tree.
    ///
    /// Reached when a declaration's name is malformed or half-typed, or when the construct is one the
    /// grammar recovered from. Reported rather than skipped: a declaration with an unreadable name still
    /// exists, and dropping it would make the scope look empty where it is merely damaged.
    UnparsableName,
    /// The macro environment is incomplete, so a condition about this name cannot be decided.
    ///
    /// The state a header is analysed in when nothing is known about its includer — see
    /// [`crate::graph::FileEntry::missing_context`]. A name that the includer might define must not be
    /// reported as absent.
    IncompleteMacroContext,
    /// The name is not declared anywhere this file can see, so the declaration is in another file.
    ///
    /// The everyday reason for a single-file analysis, and the one that keeps "not here" apart from "nowhere":
    /// `values.push_back(1)` where `values` is a `std::vector` has an answer, and it is not in this translation
    /// unit. It carries the spelling so that a message can name what was not resolved.
    ///
    /// Distinct from [`UnknownReason::UnresolvedInclude`], which is about a *specific* `#include` that could not
    /// be found: here nothing is broken, the file simply does not contain the declaration — and an analysis that
    /// had followed its includes might well have found it.
    NotDeclaredHere(Box<str>),
    /// Several declarations are visible and nothing chooses between them.
    ///
    /// Overloads, a name declared in two headers a file includes, a bare name declared in two namespaces. The
    /// answer is not "the first one": picking would make a definition jump silently land on one of several
    /// entities, and a consumer that wants to *show* the choice needs the set, not one member of it.
    ///
    /// Boxed spelling, like the other reasons that carry a name, so that this type stays small enough to sit in
    /// a `Known<T>` that is almost always `Yes`.
    Ambiguous(Box<str>),
    /// The name was a macro above this point and an `#undef` has ended it.
    ///
    /// A positive finding rather than a gap: the name is an ordinary identifier *here*, and the answer a consumer
    /// wants is "there is nothing to jump to, and here is why" — which is why it is not [`UnknownReason::No`] and
    /// not a pointer at the `#define` that is no longer in force. It is `Unknown` rather than `Known::No` because
    /// the index sees a subset of the translation unit: a header included after the `#undef` could define the name
    /// again.
    ///
    /// [`UnknownReason::No`]: crate::Known::No
    UndefinedHere(Box<str>),
    /// The type of an expression could not be worked out.
    ///
    /// The first reason in this vocabulary that is about a *type* rather than a name, and it arrived with the
    /// first query that needs one: `widget.size` cannot be answered without knowing what `widget` is, and the
    /// spelling is carried so that a message can say which expression it gave up on. Distinct from the name
    /// reasons because the fix is different — nothing is missing from the index, the analysis simply does not
    /// infer types for this shape yet.
    UnknownType(Box<str>),
}

impl UnknownReason {
    /// A sentence a diagnostic or a log line can show.
    ///
    /// Written to say what is *missing*, not merely that something is — the same rule the module and include
    /// layers follow, and for the same reason: "I could not tell" is only actionable if it says what would
    /// have made it decidable.
    pub fn describe(&self) -> String {
        match self {
            UnknownReason::MacroExpansion(name) => {
                format!(
                    "the name comes from the expansion of `{name}`, which was not expanded here"
                )
            }
            UnknownReason::ConditionalCompilation => {
                "the name is behind a condition that could not be evaluated".to_string()
            }
            UnknownReason::UnresolvedInclude(name) => {
                format!("the name may be declared in `{name}`, which was not found")
            }
            UnknownReason::UnresolvedModule(name) => format!(
                "the name may be exported by module `{name}`, which is not part of this analysis"
            ),
            UnknownReason::DependentName => {
                "the name depends on a template argument that is not known yet".to_string()
            }
            UnknownReason::UnexpandedTemplate => {
                "the declaration is inside a template that has not been instantiated".to_string()
            }
            UnknownReason::UnparsableName => {
                "the declared name could not be read from the source".to_string()
            }
            UnknownReason::IncompleteMacroContext => {
                "this file's macro environment is not fully known".to_string()
            }
            UnknownReason::NotDeclaredHere(name) => format!(
                "`{name}` is not declared in this file, so its declaration is in a file that was not read"
            ),
            UnknownReason::Ambiguous(name) => format!(
                "`{name}` is declared more than once in what this file can see, and nothing here chooses \
                 between the declarations"
            ),
            UnknownReason::UndefinedHere(name) => format!(
                "`{name}` is a macro that an `#undef` above this point has ended, so this is an ordinary \
                 identifier"
            ),
            UnknownReason::UnknownType(written) => format!(
                "the type of `{written}` is not known here, so what its members are is not known either"
            ),
        }
    }
}

/// What kind of thing a name names.
///
/// The variants are not decoration: they decide whether ordinary lookup finds the name at all, which is the
/// single most consequential question a lookup asks. See [`Name::is_ordinary`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NameKind {
    /// An ordinary identifier: `Widget`, `count`, `std`.
    ///
    /// Also the kind of a **constructor**, whose declared name really is the class name — what distinguishes
    /// it is [`BindingKind::Constructor`], not a different spelling.
    Identifier(String),
    /// A destructor: `~Widget`.
    ///
    /// The spelling includes the `~`, because that is what makes it a destructor rather than the class, and
    /// ordinary lookup ignores it — `~Widget` is only found by an explicit destructor call.
    Destructor(String),
    /// A conversion function: `operator int`, `operator std::string`.
    ///
    /// The spelling is the type as written, whitespace removed, so `operator  int` and `operator int` are one
    /// name rather than two.
    Conversion(String),
    /// An overloadable operator: `operator+`, `operator[]`, `operator()`, `operator new`.
    ///
    /// The spelling is the operator as it appears after the keyword (`+`, `[]`, `()`, `new`), not the whole
    /// `operator+`, so that a consumer can compare it against an expression's operator without re-parsing.
    Operator(String),
    /// A user-defined literal operator: `operator""_km`.
    ///
    /// The spelling is the suffix without the quotes or the `_`, so `operator""_km` and `operator"" _km` are
    /// one name — they are the same operator, and the standard treats them as such.
    Literal(String),
}

impl NameKind {
    /// The spelling as it should be shown to a user.
    ///
    /// `operator` and the quotes around a literal suffix are put back, because a consumer showing `+` where
    /// the source says `operator+` would be showing something the user cannot find.
    ///
    /// The space after `operator` is conditional, and getting it wrong is visible in every message: the
    /// symbolic forms are written closed up (`operator+`, `operator[]`, `operator()`) while the word forms
    /// take a space (`operator new`, `operator delete`), because `operatornew` is not a spelling that appears
    /// anywhere in C++.
    pub fn text(&self) -> String {
        match self {
            NameKind::Identifier(name) => name.clone(),
            NameKind::Destructor(name) => format!("~{name}"),
            NameKind::Conversion(type_name) => format!("operator {type_name}"),
            NameKind::Operator(symbol) => {
                if symbol.starts_with(|first: char| first.is_alphanumeric() || first == '_') {
                    format!("operator {symbol}")
                } else {
                    format!("operator{symbol}")
                }
            }
            NameKind::Literal(suffix) => format!("operator\"\"_{suffix}"),
        }
    }
}

/// A name, as a lookup key rather than as a string.
///
/// Ordered and hashable so that a scope's declarations can live in a sorted vector or a map — the two
/// structures a scope needs, and the reason `NameKind` carries `String` rather than a borrow: a scope
/// outlives the source text it was read from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name {
    pub kind: NameKind,
}

impl Name {
    /// An ordinary identifier.
    pub fn identifier(text: impl Into<String>) -> Self {
        Name {
            kind: NameKind::Identifier(text.into()),
        }
    }

    pub fn destructor(class: impl Into<String>) -> Self {
        Name {
            kind: NameKind::Destructor(class.into()),
        }
    }

    pub fn conversion(type_name: impl Into<String>) -> Self {
        Name {
            kind: NameKind::Conversion(type_name.into()),
        }
    }

    pub fn operator(symbol: impl Into<String>) -> Self {
        Name {
            kind: NameKind::Operator(symbol.into()),
        }
    }

    pub fn literal(suffix: impl Into<String>) -> Self {
        Name {
            kind: NameKind::Literal(suffix.into()),
        }
    }

    /// The spelling a user would recognise.
    pub fn text(&self) -> String {
        self.kind.text()
    }

    /// Is this name found by ordinary unqualified lookup?
    ///
    /// False for everything except an identifier and a destructor, and the exception is the subtle one: a
    /// **constructor** is an identifier — its declared name is the class name — so it *is* found by ordinary
    /// lookup, and what stops `Widget x;` from resolving to it is that a constructor has no return type, not
    /// its name. A destructor, by contrast, is spelled `~Widget` and is invisible to ordinary lookup
    /// entirely: `~Widget` is only reached through an explicit call.
    ///
    /// A conversion function, an operator, and a literal are likewise never found by an ordinary name
    /// lookup — `operator+` is found by looking for `+` in an expression's context, which is a different
    /// question with a different answer (overload resolution, which this layer does not do).
    pub fn is_ordinary(&self) -> bool {
        matches!(self.kind, NameKind::Identifier(_) | NameKind::Destructor(_))
    }

    /// The identifier, when this name has one.
    ///
    /// `None` for a destructor: `~Widget` names no identifier, which is why [`Name::is_ordinary`] cannot be
    /// implemented in terms of this.
    pub fn identifier_text(&self) -> Option<&str> {
        match &self.kind {
            NameKind::Identifier(name) => Some(name),
            _ => None,
        }
    }

    /// The class a destructor destroys, or the identifier when there is one.
    pub fn base_text(&self) -> Option<&str> {
        match &self.kind {
            NameKind::Identifier(name) | NameKind::Destructor(name) => Some(name),
            _ => None,
        }
    }
}

/// A dotted or qualified name: `std`, `my.mod`, `ns::Inner`.
///
/// The components are kept unresolved rather than interned into one string, because resolution is exactly the
/// question this layer defers: `ns::Inner` needs a lookup for `ns` before `Inner` means anything, and
/// flattening it to `"ns::Inner"` would make that lookup impossible while looking like it had been done.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualifiedName {
    pub components: Vec<String>,
}

impl QualifiedName {
    pub fn single(component: impl Into<String>) -> Self {
        QualifiedName {
            components: vec![component.into()],
        }
    }

    pub fn from_components(components: impl IntoIterator<Item = String>) -> Self {
        QualifiedName {
            components: components.into_iter().collect(),
        }
    }

    /// The component the name ends with — what an unqualified reference would use.
    pub fn last(&self) -> Option<&str> {
        self.components.last().map(String::as_str)
    }

    /// The parts before the last, which have to be resolved for the whole name to mean anything.
    pub fn qualifiers(&self) -> &[String] {
        let end = self.components.len().saturating_sub(1);
        &self.components[..end]
    }

    /// The name as written, with `::` between components.
    pub fn text(&self) -> String {
        self.components.join("::")
    }

    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }
}

/// A header unit name: `<vector>` or `"local.h"`.
///
/// The spelling is kept because it decides which header is meant: the quoted form searches the including
/// file's own directory first and the angle form does not, so `"vector"` and `<vector>` can be two different
/// files.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HeaderName {
    pub name: String,
    pub is_angle: bool,
}

impl HeaderName {
    pub fn text(&self) -> String {
        if self.is_angle {
            format!("<{}>", self.name)
        } else {
            format!("\"{}\"", self.name)
        }
    }
}

/// The name a declaration declares.
///
/// One variant per *shape*, because the shapes are resolved by different rules — see [`Name::is_ordinary`].
/// `None` is a real case and not an error: `static_assert`, an anonymous namespace, and a malformed
/// declaration all declare something with no name, and a model that made a name mandatory would have to
/// invent one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeclName {
    /// A name that introduces an entity: `int count;`, `namespace ns {`, `class Widget {`.
    Named(Name),
    /// A qualified name: `int Inner::count;`, `void ns::f();`.
    ///
    /// Kept separate because a consumer has to resolve the qualifier before the name means anything, and
    /// doing that is a lookup rather than a string operation.
    Qualified(QualifiedName),
    /// A **module** name: `export module my.mod;`, `import std;`.
    Module(QualifiedName),
    /// A **header unit** name: `import <vector>;`.
    HeaderUnit(HeaderName),
    /// A **partition** of the enclosing module: `module my.mod:part;`, `import :part;`.
    Partition(QualifiedName),
}

impl DeclName {
    /// The name as it should be shown to a user.
    pub fn text(&self) -> String {
        match self {
            DeclName::Named(name) => name.text(),
            DeclName::Qualified(qualified) => qualified.text(),
            DeclName::Module(module) => module.text(),
            DeclName::HeaderUnit(header) => header.text(),
            DeclName::Partition(partition) => format!(":{}", partition.text()),
        }
    }

    /// The name as an ordinary lookup key, for the kinds that have one.
    ///
    /// `None` for a qualified name — the qualifier has to be resolved first — and for the module-shaped
    /// names, which are matched against the module graph rather than against a scope.
    pub fn ordinary_name(&self) -> Option<&Name> {
        match self {
            DeclName::Named(name) if name.is_ordinary() => Some(name),
            _ => None,
        }
    }
}

/// The name of a declaration or an import, when there is one.
///
/// A type alias rather than a wrapper, because "this declaration has no name" is a fact about the program and
/// not a failure of the analysis: `namespace { }` and `static_assert(...)` are ordinary C++, and a consumer
/// has to handle an unnamed declaration either way. Keeping it an `Option` means an unnamed declaration
/// cannot be confused with a name that could not be *read*, which is
/// [`UnknownReason::UnparsableName`].
pub type MaybeName = Option<DeclName>;

/// What a binding binds, as far as the syntax can say.
///
/// Derived from the *shape* of the declaration and not from types, because at this layer there are no types.
/// `Class` for `class Widget` and `Template` for `template <...> class Widget` are the two that matter most,
/// and both are read straight off the declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BindingKind {
    Namespace,
    /// `class`, `struct`, or `union` — the tag kinds, which share one name space in C++.
    Class,
    Enum,
    /// An enumerator: the `Red` in `enum { Red }`.
    Enumerator,
    Function,
    /// A constructor. Its [`Name`] is an identifier spelling the class name, because that is genuinely how it
    /// is spelled.
    Constructor,
    /// A destructor. Its [`Name`] carries the `~`.
    Destructor,
    /// A conversion function: `operator int`.
    ConversionFunction,
    /// An overloaded operator: `operator+`.
    OperatorFunction,
    /// A user-defined literal operator: `operator""_km`.
    LiteralOperator,
    /// A variable, including a function parameter, a field, and a structured binding.
    Variable,
    /// A `using` declaration that names an existing entity: `using ns::f;`.
    UsingDeclaration,
    /// A `using` directive: `using namespace ns;`, which makes a whole namespace's names visible without
    /// declaring any of them.
    UsingDirective,
    /// A type or namespace alias: `using Int = int;`, `namespace fs = std::filesystem;`.
    Alias,
    /// A `typedef`.
    Typedef,
    /// A template parameter: the `T` in `template <typename T>`, or the non-type `N` in `template <int N>`.
    ///
    /// Its own kind rather than an alias, because it is declared in a scope of its own — the template
    /// parameter list — and shadows nothing outside the declaration it belongs to. A consumer listing what a
    /// template declares has to find it, and a consumer resolving `T::value_type` has to know it is a
    /// parameter rather than a real type: that is the difference between a name that can be looked up now and
    /// one that is [`UnknownReason::DependentName`].
    TemplateParameter,
    /// A label, which is in a name space of its own and is never found by ordinary lookup.
    Label,
    /// A deduction guide, a `static_assert`'s name, or anything else the syntax identified as a declaration
    /// without a more specific shape.
    Other,
}

impl BindingKind {
    /// Is this binding a type, as far as its shape says?
    ///
    /// What a consumer keying on "the thing after `::` must be a type or a namespace" needs. Deliberately
    /// shape-based: a `typedef` and an alias are types, a variable is not, and a template is not known to be
    /// either until its arguments are.
    pub fn is_type_like(self) -> bool {
        matches!(
            self,
            BindingKind::Class
                | BindingKind::Enum
                | BindingKind::Alias
                | BindingKind::Typedef
                | BindingKind::Namespace
        )
    }

    /// Does this binding introduce a scope of its own?
    pub fn opens_a_scope(self) -> bool {
        matches!(
            self,
            BindingKind::Namespace | BindingKind::Class | BindingKind::Enum | BindingKind::Function
        )
    }
}

/// A name bound to a declaration, in a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: Name,
    pub kind: BindingKind,
    /// The whole declaration's range — what a go-to-definition highlights and what a rename replaces.
    pub range: SourceRange,
    /// Just the name's range.
    ///
    /// Separate from `range` because the two are needed at different times: a reference search matches the
    /// *name*, while a definition jump highlights the *declaration*. Collapsing them is the difference
    /// between a rename that edits one identifier and one that deletes the whole declaration.
    pub name_range: SourceRange,
    pub scope: ScopeId,
    /// Where the declaration came from, when it did not come from this file's own text.
    ///
    /// `None` for a declaration written here. The macro case is the one that matters: a binding produced by
    /// `DEFINE_FOO(x)` has a range inside the macro's expansion, and a consumer that treated it as ordinary
    /// text would offer a rename that edits the macro definition's arguments.
    ///
    /// Distinct from [`crate::expand::Origin`], which answers the same-shaped question for a *token*. The two
    /// cannot be one type: expansion tracks a whole invocation chain because a token can come from a macro
    /// that was written by another macro, while a binding is either written in this file or reached through
    /// one `#include`, and recording more here would imply a precision the symbol layer does not have.
    pub origin: Option<BindingOrigin>,
}

/// Where a binding came from, when that is not simply "here".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingOrigin {
    /// Produced by a macro expansion. The name is the macro's.
    ///
    /// The invocation's range is where a consumer should send the user, because that is the text they can
    /// edit — see [`crate::expand::Expansion`], where the same rule is applied to tokens.
    MacroExpansion {
        macro_name: String,
        definition: SourceRange,
    },
    /// Declared in an `#include`d file and visible here because inclusion is textual.
    ///
    /// The file is recorded because the binding's range is meaningless without it: an offset into a header is
    /// an offset into *that* header, not into the file being analysed.
    Included { file: FileId },
}

/// What a scope is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeKind {
    /// The translation unit itself. Every file has exactly one, including a header — a header's file scope is
    /// where its top-level declarations live when it is analysed on its own.
    TranslationUnit,
    Namespace,
    /// Also a class, struct, or union. One variant because the difference is access, which is a member's
    /// property rather than the scope's.
    Class,
    Enum,
    /// A function's body, and its parameter scope — they differ only in what is bound, not in how lookup
    /// works.
    Function,
    /// A statement block: `{ ... }`, a loop body, a branch.
    Block,
    /// A template's parameter list, which is in scope for the declaration that follows.
    TemplateParameters,
    /// A lambda's body, which introduces a scope of its own and captures names into it.
    Lambda,
}

/// A region of the program in which names are declared and looked up.
///
/// The tree mirrors the syntax tree rather than being derived from it lazily: a scope corresponds to a node,
/// and keeping the correspondence explicit is what lets a consumer at a cursor position find its scope
/// without re-walking the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub children: Vec<ScopeId>,
    pub kind: ScopeKind,
    /// The name this scope *introduces*: `ns` for `namespace ns { }`, `Widget` for `struct Widget { }`.
    ///
    /// A property of the scope rather than of the binding, because the two answer different questions. The
    /// binding says "`Widget` is declared in the enclosing scope" — which is where a consumer looks the name
    /// up — while this says "inside here, the current entity is `Widget`". Only the second can produce a
    /// **qualified** name, and a qualified name is what an index keys on: `ns::C::f` cannot be recovered from
    /// the binding of `f` alone, and it cannot be recovered from the enclosing scope either, because the
    /// scope does not know what it is called.
    ///
    /// `None` where the construct introduces no name: a function body, a statement block, a template parameter
    /// list, a lambda, an anonymous namespace, and the translation unit. Those are exactly the scopes that
    /// contribute nothing to a qualified name, which is why [`ScopeTree::qualified_name_of`] skips them.
    pub name: Option<Box<str>>,
    /// The syntax node this scope came from, as a range. `None` for a scope the model synthesised, such as
    /// the file scope of an empty file.
    pub range: Option<SourceRange>,
    /// The declarations written directly in this scope, by name.
    ///
    /// A `Vec` rather than a map because a name may be declared more than once — overloads, redeclarations, a
    /// variable shadowing a function — and returning only one of them would be a resolution decision this
    /// layer is not entitled to make. Ordered by name so a lookup can binary-search it.
    pub bindings: Vec<Binding>,
}

impl Scope {
    /// Every binding of a name in this scope, in declaration order.
    ///
    /// **This scope only.** It does not walk outward, because outward lookup is not "the parent scope" in
    /// C++: a class's base classes, a `using namespace` directive, and argument-dependent lookup all
    /// contribute, and a method that silently did only the parent walk would make a consumer's call site look
    /// complete when it is not. Outward lookup is a separate step with its own signature.
    pub fn bindings_of(&self, name: &Name) -> impl Iterator<Item = &Binding> {
        self.bindings
            .iter()
            .filter(move |binding| &binding.name == name)
    }

    /// Every name declared directly in this scope, deduplicated and sorted.
    pub fn declared_names(&self) -> Vec<&Name> {
        let mut names: Vec<&Name> = self.bindings.iter().map(|binding| &binding.name).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Is this scope empty of declarations?
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// A scope's identity.
///
/// Indices into a [`ScopeTree`] rather than pointers, for the same reason [`FileId`] is an index: the table
/// owns the scopes, so a `ScopeId` that outlives one table cannot be silently valid in another. A pointer
/// would make that mistake possible and a reference would make the table unbuildable while it is being
/// filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(pub usize);

impl ScopeId {
    pub fn index(self) -> usize {
        self.0
    }
}

/// One file's scopes and bindings.
///
/// The unit the analysis layer works in, and the reason it can answer the most frequent editor questions
/// without consulting anything else: completion at a cursor needs this file's scopes, its own preprocessor
/// state, and nothing more. A table that required the project to be walked first would make every keystroke
/// cost the whole project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeTree {
    scopes: Vec<Scope>,
    /// The file scope, when one was created. Every non-empty file has exactly one.
    root: Option<ScopeId>,
    /// The places where a scope in this table was read from a **macro's replacement list** rather than from the
    /// file's own braces — see [`MacroScopeReading`].
    ///
    /// Kept on the table because the walk that produced the scopes is the only thing that knows, and because the
    /// step that stores a summary takes the table apart anyway. Sorted by position, which is the order the walk
    /// meets them in, so two walks of the same text produce the same list.
    pub macro_readings: Vec<MacroScopeReading>,
}

impl ScopeTree {
    pub fn new() -> Self {
        ScopeTree::default()
    }

    /// The scopes this file read out of macro bodies, in the order the walk met them.
    pub fn macro_readings(&self) -> &[MacroScopeReading] {
        &self.macro_readings
    }

    /// The file scope, if the table has one.
    pub fn root(&self) -> Option<ScopeId> {
        self.root
    }

    pub fn scope(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(id.index())
    }

    pub fn scope_mut(&mut self, id: ScopeId) -> Option<&mut Scope> {
        self.scopes.get_mut(id.index())
    }

    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    /// Create a scope, returning its id.
    ///
    /// The file scope is recognised by having no parent, so a caller cannot create two of them by accident —
    /// a second root would make "the file's declarations" ambiguous.
    ///
    /// This is the **unnamed** form: the scope introduces nothing, which is right for a function body, a block,
    /// a lambda and a template parameter list. A scope that introduces a name — `namespace ns`, `struct Widget`
    /// — is created with [`ScopeTree::create_named_scope`], because the name is what makes a qualified name
    /// reachable and there is no moment at which a scope is usefully nameless-but-shouldn't-be.
    pub fn create_scope(
        &mut self,
        kind: ScopeKind,
        parent: Option<ScopeId>,
        range: Option<SourceRange>,
    ) -> ScopeId {
        self.create_named_scope(kind, parent, range, None)
    }

    /// [`ScopeTree::create_scope`] for a scope that **introduces a name**: `namespace ns`, `struct Widget`.
    ///
    /// The name is the *introduced* segment, not a `::`-qualified spelling: `namespace a::b` creates one scope
    /// per level and each records its own part, which is what makes `namespace a::b {` and
    /// `namespace a { namespace b {` produce the same qualified name. Joining the segments is
    /// [`ScopeTree::qualified_name_of`]'s job.
    pub fn create_named_scope(
        &mut self,
        kind: ScopeKind,
        parent: Option<ScopeId>,
        range: Option<SourceRange>,
        name: Option<&str>,
    ) -> ScopeId {
        let id = ScopeId(self.scopes.len());

        self.scopes.push(Scope {
            parent,
            children: Vec::new(),
            kind,
            name: name.map(Box::from),
            range,
            bindings: Vec::new(),
        });

        match parent {
            Some(parent) => {
                if let Some(scope) = self.scopes.get_mut(parent.index()) {
                    scope.children.push(id);
                }
            }
            None => {
                if self.root.is_none() {
                    self.root = Some(id);
                }
            }
        }

        id
    }

    /// The `::`-qualified name of what a scope introduces, outermost segment first.
    ///
    /// `Some("ns::C")` for the body of `namespace ns { struct C { … }; }`, and `None` for a scope that
    /// introduces no name at all: a function body, a block, a lambda, the translation unit.
    ///
    /// # Which scopes contribute a segment
    ///
    /// A name-bearing scope contributes its own name; a scope that bears no name contributes nothing *and
    /// stops nothing* — walking past it continues to whatever encloses it. That is what makes `void ns::f() {`
    /// and `void f() {` agree: the function body is transparent in both, and only the namespace the function
    /// was written in is part of the answer.
    ///
    /// The one kind that must **stop** the walk rather than pass through is an unnamed [`ScopeKind::Class`] or
    /// [`ScopeKind::Enum`]: what a member declares is `C::m` and never `ns::m`, so a class with no name of its
    /// own is a dead end — it has no segment to contribute and none to inherit, and continuing past it would
    /// invent a qualification that does not exist.
    pub fn qualified_name_of(&self, scope: ScopeId) -> Option<String> {
        let mut segments: Vec<&str> = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(scope);

        while let Some(id) = current {
            // A cycle cannot occur in a table this type builds, but a hand-constructed one could — the same
            // guard `scope_chain` carries, for the same reason.
            if !seen.insert(id) {
                break;
            }

            let Some(scope) = self.scopes.get(id.index()) else {
                break;
            };

            match &scope.name {
                Some(name) => segments.push(name),
                None if matches!(scope.kind, ScopeKind::Class | ScopeKind::Enum) => break,
                None => {}
            }

            current = scope.parent;
        }

        if segments.is_empty() {
            return None;
        }

        segments.reverse();
        Some(segments.join("::"))
    }

    /// Add a binding to a scope.
    ///
    /// `false` when the scope does not exist, or when its [`ScopeKind`] cannot hold a binding of this kind —
    /// a label in a namespace, say. Reported rather than panicking because this layer runs on malformed
    /// input, and a rejected binding is a gap a consumer can see while a panic is an editor that stops
    /// working.
    pub fn add_binding(&mut self, scope: ScopeId, binding: Binding) -> bool {
        let Some(target) = self.scopes.get_mut(scope.index()) else {
            return false;
        };

        if !accepts_binding(target.kind, binding.kind) {
            return false;
        }

        // Kept sorted by name so a lookup can binary-search, and stable within a name so that declaration
        // order survives — a consumer showing overloads shows them in the order they were written.
        let position = target
            .bindings
            .partition_point(|existing| existing.name <= binding.name);

        target.bindings.insert(position, binding);
        true
    }

    /// The prefix a declaration written **in** this scope is qualified by: the scope's own qualified name,
    /// for a scope that is part of a qualified name at all.
    ///
    /// The difference from [`ScopeTree::qualified_name_of`] is one question: does the scope's own name count?
    /// For the scope that *introduces* an entity it does — `namespace ns` introduces `ns`, so a declaration
    /// written in it is `ns::x`. For a scope that merely *holds* declarations it does not: a member of
    /// `struct C` is `C::member` and not `C::C::member`, and a local in `void f()` is `local` and not `f::local`
    /// — or worse, `ns::f::local`, since a function body's name is not a scope a name can be written from.
    ///
    /// So the same set of transparent scopes is skipped, and the answer is then used as a **prefix** rather
    /// than as the name itself. A file scope and a function body both end up with `None`, which is the same
    /// answer and for the same reason: neither contributes a segment, and neither has a name to start from.
    pub fn qualification_prefix_of(&self, scope: ScopeId) -> Option<String> {
        match self.scopes.get(scope.index())?.kind {
            // A scope whose own name is the first segment of everything written inside it.
            ScopeKind::Namespace | ScopeKind::Class | ScopeKind::Enum => {
                self.qualified_name_of(scope)
            }
            // A file, a function body, a block, a lambda, a template parameter list: declarations written here
            // are not qualified by anything this scope knows, and walking outward would attribute them to an
            // enclosing entity they are not part of.
            ScopeKind::TranslationUnit
            | ScopeKind::Function
            | ScopeKind::Block
            | ScopeKind::Lambda
            | ScopeKind::TemplateParameters => None,
        }
    }

    /// The scope that contains `offset`, innermost first.
    ///
    /// "Innermost" is the scope with the smallest range containing the offset, which is what a consumer at a
    /// cursor position needs. `None` when the table has no scope covering the offset — an offset past the end
    /// of the file, or a table built from a different source.
    pub fn scope_at(&self, offset: usize) -> Option<ScopeId> {
        self.scopes
            .iter()
            .enumerate()
            .filter(|(_, scope)| {
                scope.range.is_some_and(|range| {
                    offset >= range.start_offset && offset <= range.end_offset()
                })
            })
            // Smallest range wins: the innermost scope containing the offset. Equal ranges cannot happen for
            // distinct scopes, because each comes from a distinct node.
            .min_by_key(|(_, scope)| scope.range.map(|range| range.length))
            .map(|(index, _)| ScopeId(index))
    }

    /// The chain from a scope outward to the file scope, innermost first.
    ///
    /// Innermost-first because that is the order C++ lookup considers them, so a consumer walking the chain
    /// is walking in the order the language does. It stops at the file scope: crossing into another file is
    /// the include graph's business, not this table's.
    pub fn scope_chain(&self, from: ScopeId) -> Vec<ScopeId> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(from);

        while let Some(id) = current {
            // A cycle cannot occur in a table this type builds, but a hand-constructed one could, and a
            // consumer that hung on it would be worse than one that stopped early.
            if !seen.insert(id) {
                break;
            }

            let Some(scope) = self.scopes.get(id.index()) else {
                break;
            };

            chain.push(id);
            current = scope.parent;
        }

        chain
    }

    /// The scope a `::`-qualified spelling names: `ns::C` for the body of `namespace ns { struct C { … }; }`.
    ///
    /// The counterpart of [`ScopeTree::qualified_name_of`], and the reason a *qualified* name can be resolved at
    /// all: `ns::C::f` is not a name to look up, it is a scope to find and then a name to look up **in it**. The
    /// spelling is compared against the joined segments rather than parsed, so `namespace a::b {` and
    /// `namespace a { namespace b {` — which produce the same spelling — are found by the same query.
    ///
    /// A linear scan, which is what a per-file table of a few hundred scopes can afford: a query asks this once
    /// per segment of one name, and building an index for it would cost more than the scans it saves.
    ///
    /// `None` when no scope bears that name — including for the empty spelling, which no scope records: the file
    /// scope has no name of its own and is reached through [`ScopeTree::root`].
    pub fn scope_with_qualified_name(&self, name: &str) -> Option<ScopeId> {
        (0..self.scopes.len())
            .map(ScopeId)
            .find(|id| self.qualified_name_of(*id).as_deref() == Some(name))
    }

    /// Is a declaration written in this scope **local** — nameable only from inside the body it sits in?
    ///
    /// True when the chain outward reaches a function body, a block or a lambda, however far in it is: a local
    /// class, a `typedef` in a loop, a variable in a member function. False at file scope, in a namespace, and in a
    /// class body — a *member* function's declaration is not local, only its body is.
    ///
    /// The three kinds are the ones whose name reaches no qualified name ([`ScopeTree::qualification_prefix_of`]
    /// lists the same set from the other side): a function, a block and a lambda each introduce no segment, so a
    /// declaration inside one has a bare name and no scope to be found in. What this adds is the thing a consumer
    /// needs and a qualified name cannot say: *which* of the two `None`-scoped cases a declaration is.
    ///
    /// The file scope is deliberately not "local": a declaration written directly at file scope is visible to every
    /// file that includes this one, which is the whole distinction.
    pub fn declares_a_local(&self, scope: ScopeId) -> bool {
        self.scope_chain(scope).iter().any(|id| {
            matches!(
                self.scopes.get(id.index()).map(|scope| scope.kind),
                Some(ScopeKind::Function | ScopeKind::Block | ScopeKind::Lambda)
            )
        })
    }
}

/// Can a scope of this kind hold a binding of that kind?
///
/// The rules are small and worth writing down because getting them wrong is silent: a label added to a
/// namespace would make `goto` completion offer names that cannot be jumped to, and a namespace added to a
/// function would make `namespace ns { }` inside a body look like it declared something at file scope.
///
/// A label is only legal in a function or a block; a namespace is only legal at file scope or inside another
/// namespace; and a template parameter list holds template parameters and nothing else, because it is the one
/// scope in C++ that is not a place for a declaration.
fn accepts_binding(scope: ScopeKind, binding: BindingKind) -> bool {
    match binding {
        BindingKind::Label => matches!(
            scope,
            ScopeKind::Function | ScopeKind::Block | ScopeKind::Lambda
        ),
        BindingKind::Namespace | BindingKind::UsingDirective => {
            matches!(scope, ScopeKind::TranslationUnit | ScopeKind::Namespace)
        }
        BindingKind::TemplateParameter => matches!(scope, ScopeKind::TemplateParameters),
        // A template parameter list is the one scope a declaration cannot appear in: `template <int N, int x;>`
        // is not a thing, and a `Variable` binding there is a sign the extractor misread a default argument.
        // Everything else is legal wherever a declaration is.
        _ => !matches!(scope, ScopeKind::TemplateParameters),
    }
}
