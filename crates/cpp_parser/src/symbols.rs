//! The **external symbol table**: what a caller can tell the parser about names it has already resolved.
//!
//! # Why the parser wants one, and why it can do without
//!
//! Several parse questions in C++ are settled by looking a name up, and the two answers are the *same tokens*:
//!
//! ```text
//! Widget w(1, 2);    a variable initialised with two arguments — `Widget` is a type
//! g(1, 2);           a call with two arguments — `g` is a function
//!
//! MY_API Widget *p;  a declaration whose macro expands to a specifier
//! NUMBER_OPTION(x)   a macro invocation whose body is a whole statement, so no `;` follows
//! g(x)               a call with its `;` missing
//! ```
//!
//! A compiler resolves the name; this parser cannot, because it never sees another translation unit and does not
//! run the preprocessor. What it does instead is **decide from what the file says about itself** — the tables in
//! [`crate::parser::TypeNames`] and [`crate::parser::MacroNames`] — and, when the file says nothing, fall back to
//! shape preferences that are documented and pinned by tests. That is what makes the parser usable on a single
//! buffer with no context at all, and it is why it is **total**: every input produces a lossless, well-formed tree.
//!
//! A caller that *does* have more context — an editor with a project index, or `cpp_code_analysis` once it can
//! build one — can hand that context in through this trait, and the parser will prefer it to its own guesses.
//! Both halves are needed, and neither replaces the other:
//!
//! * the table knows what this file cannot (headers, other files, macros expanded elsewhere);
//! * the heuristics are what remains when the table is absent, stale, or simply does not know — which is the
//!   ordinary state of affairs while a file is being typed.
//!
//! # The three answers, and why `bool` is not enough
//!
//! [`SymbolTable::kind_of`] returns `Option<SymbolKind>`, and a `None` means **"this table does not know"** —
//! *not* "this name is not a type". Nothing in this interface can say "no": a table that knows `g` is a function
//! answers `Some(SymbolKind::Function)`, which is what refuses the declaration reading; a table that has never
//! heard of `Widget` answers `None`, and the parser falls back to its own evidence. Collapsing the two into a
//! `bool` would make a stale index silently change how valid code parses, which is the one failure mode this
//! crate's `A0` class is about.
//!
//! # Precedence, and the staleness that motivates it
//!
//! Callers of the grammar apply this order, and it is deliberate:
//!
//! 1. **what this file says** — [`crate::parser::TypeNames`], [`crate::parser::MacroNames`]: a name declared or
//!    `#define`d in the text being parsed. Always the freshest evidence there is, because it *is* the text;
//! 2. **the external table** — everything the file cannot see;
//! 3. **the shape preferences** — the documented guesses, unchanged.
//!
//! A deferred index lags the buffer, so it is consulted last among the sources of evidence rather than first: a
//! name the user has just renamed is answered by (1) or by nothing at all, and never by a stale (2).
//!
//! # What an implementation must guarantee
//!
//! * **It decides readings, not structure.** Whatever a table answers — including nonsense — the parse stays
//!   lossless and well-formed. A table can make the parser choose the wrong reading; it cannot make it drop
//!   tokens.
//! * **It is a pure function of the name.** `&self`, no interior state that changes an answer, no I/O: the same
//!   source and the same table must produce the same tree every time (`I3`), and the parser may ask thousands of
//!   times per file. `None` is always a safe answer, and an empty table must be indistinguishable from no table.
//! * **It is shared, not owned.** `Send + Sync`, so an editor can hold one index and parse several buffers
//!   against it.
//! * **It needs no checkpointing.** Because it is read-only and external, a speculative parse that is rolled back
//!   needs no restoring of anything — unlike the parser's own tables, which is why those live in `Checkpoint` and
//!   this one does not.
//!
//! # The format lives here
//!
//! The trait and its vocabulary are defined by the *consumer* — this crate — and implemented by whoever has the
//! index. That keeps the dependency pointing one way: the parser never learns what an index is, and
//! `cpp_code_analysis` never has to expose its internals. Extending the vocabulary means adding a variant or a
//! method with a default implementation, so an existing implementation keeps compiling and simply answers `None`
//! for what it does not know — which is exactly the fallback above.

/// What a caller can tell the parser about a name it has already resolved.
///
/// Implemented by whoever has the index — `cpp_code_analysis`, or a test. See the module documentation for the
/// contract, and [`NoSymbols`] / [`SymbolMap`] for the two implementations that ship with this crate.
pub trait SymbolTable: Send + Sync {
    /// What is `name`, as far as this table knows?
    ///
    /// `None` means **"this table does not know"**, never "this name is not a type" — see the module
    /// documentation. A name is asked for **as it is written** in the source, without qualification: resolving
    /// `f` inside `namespace n` to `n::f` is the implementation's business, not the parser's, which knows only
    /// the spelling in front of it.
    fn kind_of(&self, name: &str) -> Option<SymbolKind>;
}

/// A table that knows nothing.
///
/// Parsing against it is **exactly** parsing with no table at all — the equivalence is asserted in
/// `tests/symbols.rs`, which is what keeps "the table is optional" a fact rather than an intention. It exists so
/// that a caller can pass *something* (`Option<&dyn SymbolTable>` is otherwise awkward to fill) and so that the
/// equivalence test has a second operand.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSymbols;

impl SymbolTable for NoSymbols {
    fn kind_of(&self, _name: &str) -> Option<SymbolKind> {
        None
    }
}

/// A table backed by a map — the reference implementation, for tests and for a caller whose index is simple
/// enough that it does not need a type of its own.
///
/// ```no_run
/// use cpp_parser::{MacroBody, ParserConfig, SymbolKind, SymbolMap};
///
/// let mut symbols = SymbolMap::new();
/// symbols.insert("MY_API", SymbolKind::Macro { function_like: false, body: MacroBody::Specifier });
/// symbols.insert("Widget", SymbolKind::Type);
///
/// let config = ParserConfig::default().with_symbol_table(&symbols);
/// ```
#[derive(Debug, Default, Clone)]
pub struct SymbolMap {
    entries: std::collections::HashMap<Box<str>, SymbolKind>,
}

impl SymbolMap {
    pub fn new() -> Self {
        SymbolMap::default()
    }

    /// Record what `name` is, replacing any earlier answer for it.
    pub fn insert(&mut self, name: &str, kind: SymbolKind) {
        self.entries.insert(name.into(), kind);
    }

    /// [`SymbolMap::insert`], for building a table in one expression.
    pub fn with(mut self, name: &str, kind: SymbolKind) -> Self {
        self.insert(name, kind);
        self
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl SymbolTable for SymbolMap {
    fn kind_of(&self, name: &str) -> Option<SymbolKind> {
        self.entries.get(name).copied()
    }
}

impl<T: SymbolTable + ?Sized> SymbolTable for &T {
    fn kind_of(&self, name: &str) -> Option<SymbolKind> {
        (**self).kind_of(name)
    }
}

/// What a name is.
///
/// The vocabulary is the parser's, not the index's: each variant is here because a *reading* turns on it, and the
/// documentation of each says which one. A symbol the parser has no use for (a label, a parameter, a
/// `using`-declaration's target) does not belong here — an implementation that cannot classify a name answers
/// `None` and leaves the fallback in charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// A class, struct, union, enum or alias — a name that can stand where a **type** goes.
    ///
    /// The single most valuable answer: it settles `Widget w(1, 2);` against `g(1, 2);`, and `(Widget)x` against
    /// `(x)`, without any of the shape heuristics that exist for the case where nobody knows.
    Type,

    /// A **template's** name: `std::vector`, a user's `Vec`.
    ///
    /// A `Type` and a template are the same name until a `<` follows it, and then they are not: a name that is
    /// known to be a template makes the `<` after it template arguments rather than a comparison, which is the
    /// question `a_matching_angle_bracket_follows` has to guess at today.
    Template,

    /// A **macro**, with the two properties the grammar can act on.
    ///
    /// A macro is expanded before the grammar runs, so an invocation leaves behind whatever its body produced —
    /// which is precisely what [`MacroBody`] describes, and the reason this variant carries data where the others
    /// do not.
    Macro {
        /// Is it invoked with arguments — `NAME(x)` — or used bare?
        ///
        /// An object-like macro (`#define MY_API __declspec(dllexport)`) stands where a *specifier* goes; a
        /// function-like one stands where a call does.
        function_like: bool,
        /// What the body expands to, as far as the table knows.
        body: MacroBody,
    },

    /// A free or member function.
    ///
    /// The **negative** answer that the parser cannot reach on its own: `g(1, 2);` is a call because `g` is a
    /// function, even where `g(1, 2)` would also parse as a declaration of a variable `g` with a direct
    /// initialiser. `None` cannot say this, which is why the vocabulary has variants the parser may only ever use
    /// to *refuse* a reading.
    Function,

    /// A variable, field, parameter or enumerator: a name that is a value, never a type.
    Variable,

    /// A namespace: a name that a `::` continues.
    Namespace,
}

/// What a macro's body expands to — the part of a macro the *grammar* cares about.
///
/// A macro whose body is unknown is still useful to know about (the name is a macro, so `NAME(x)` is not a call
/// to a function), but the shapes below are what turn a guess into a reading:
///
/// ```text
/// MacroBody::Specifier       #define MY_API __declspec(dllexport)      MY_API Widget *p;
/// MacroBody::Statement       #define NUMBER_OPTION(op) if (…) { … }   NUMBER_OPTION(x)      no `;`
/// MacroBody::Statement       #define ASSERT(c) do { } while (false)   ASSERT(x);            with `;`
/// MacroBody::Block           #define TEST(a, b) …                     TEST(A, B) { … }
/// MacroBody::Expression      #define MAX(a, b) ((a) > (b) ? (a) : (b))  auto m = MAX(1, 2);
/// MacroBody::Type            #define MY_INT int                       MY_INT x = 1;
/// ```
///
/// The distinction that earns the most is `Statement` against `Block`: both may be followed by a block, and only
/// the first may appear **without** a `;` and without anything after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroBody {
    /// Modifiers, attributes, an export marker: the macro stands where a *declaration specifier* goes.
    Specifier,

    /// A whole statement, `;` included or implied by a block of its own.
    ///
    /// The invocation may be written without a trailing `;`, and may be followed by a block that belongs to it —
    /// which is the shape `NUMBER_OPTION(x)` and `IF_EXIST(x) { … }` are.
    Statement,

    /// A statement that *requires* a block: the invocation is always followed by `{ … }`, which is part of what
    /// the macro wrote (gtest's `TEST(A, B)`, a `#define` that opens a scope).
    Block,

    /// An expression, so the invocation is an operand and nothing more: `auto m = MAX(1, 2);`.
    Expression,

    /// A type name or a type expression, so the invocation can stand where a type goes.
    Type,

    /// The table knows the name is a macro and nothing about its body.
    ///
    /// The ordinary answer for a macro defined in a header, and not a failure: it already rules out the *call*
    /// reading, which is what most of the value is.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::{MacroBody, NoSymbols, SymbolKind, SymbolMap, SymbolTable};

    #[test]
    fn a_map_answers_what_was_inserted_and_nothing_else() {
        let mut symbols = SymbolMap::new();
        symbols.insert(
            "MY_API",
            SymbolKind::Macro {
                function_like: false,
                body: MacroBody::Specifier,
            },
        );
        symbols.insert("Widget", SymbolKind::Type);

        assert_eq!(symbols.kind_of("Widget"), Some(SymbolKind::Type));
        assert!(matches!(
            symbols.kind_of("MY_API"),
            Some(SymbolKind::Macro {
                function_like: false,
                body: MacroBody::Specifier
            })
        ));
        assert_eq!(
            symbols.kind_of("g"),
            None,
            "a name the table has never heard of is unknown, not a `no`"
        );
        assert_eq!(symbols.len(), 2);
        assert!(SymbolMap::new().is_empty());
    }

    #[test]
    fn the_interface_is_object_safe_and_shared() {
        // The two shapes the parser will hold: a trait object, and a reference that outlives the parse. Both are
        // checked here because neither can be discovered from a caller's error message once the grammar uses them.
        let table: &dyn SymbolTable = &NoSymbols;
        assert_eq!(table.kind_of("anything"), None);

        let map = SymbolMap::new().with("Widget", SymbolKind::Type);
        let borrowed: &dyn SymbolTable = &map;
        assert_eq!(borrowed.kind_of("Widget"), Some(SymbolKind::Type));

        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SymbolMap>();
        assert_send_sync::<NoSymbols>();
        assert_send_sync::<SymbolKind>();
    }
}
