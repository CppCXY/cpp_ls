//! A local table of the names a translation unit declares to be **types**.
//!
//! # Why the parser needs one
//!
//! C++ decides several parse questions by looking up a name, so a parser without a symbol table has to guess —
//! and the guesses are visible in the tree:
//!
//! ```text
//! Widget w(1, 2);    a variable initialised with two arguments
//! g(1, 2);           a call with two arguments
//! ```
//!
//! Those are the same tokens. The difference is that `Widget` names a type, and no amount of token inspection
//! answers it. A compiler looks it up; an editor-facing parser can do the same for the names **the file
//! declares**, which is most of what matters in practice.
//!
//! # What it is and is not
//!
//! It is a *syntactic* table: a name is recorded because a declaration wrote it in a position where a type name
//! goes — `class Widget`, `typedef ... Integer`, `using Alias = ...`. Nothing here resolves a template, follows
//! a `typedef` to its target, or reads another file. It is a lookup table for one parse, not a semantic model,
//! and it is deliberately narrow because every name it accuses of being a type changes how the file parses.
//!
//! The failure mode is one-sided, which is why it is acceptable at all: a **missed** name costs a declaration
//! read as an expression (the variable goes unbound, and completion is worse), while a **wrong** name costs a
//! call read as a declaration (the callee and its arguments vanish, and a name enters scope that was never
//! declared). Recording too little is therefore the safe direction, and the table records only what a
//! declaration spells out.
//!
//! # Why scopes are recorded but barely enforced
//!
//! A name declared inside a function body is not a type outside it, so the table remembers the depth at which
//! each name was written and answers only for names visible at the current depth. The depth is a *parser* depth
//! — how many braced bodies are open — not a C++ scope, so it is a good approximation rather than the real
//! thing: two sibling functions share a depth, and a name declared in the first is visible while parsing the
//! second. Being approximate here is the same trade as above: the table is used to prefer one reading over
//! another, and a stale name means the declaration reading wins for a name that happened to be declared
//! somewhere else in the file — which is what a human reading the file would assume too.

/// The names a translation unit's declarations introduce as types.
///
/// See the module documentation for what is and is not recorded.
#[derive(Debug, Default, Clone)]
pub struct TypeNames {
    /// `(name, depth)`, in declaration order, appended and never removed.
    ///
    /// A `Vec` rather than a set because the depth has to be compared at lookup time, and because a file
    /// declares far fewer types than it writes tokens: a linear scan from the end is cheaper than hashing and
    /// finds the most recent declaration first, which is the one that shadows.
    bindings: Vec<(Box<str>, usize)>,
    /// How many braced bodies are open. See the module documentation for what this approximates.
    depth: usize,
}

impl TypeNames {
    pub fn new() -> Self {
        TypeNames::default()
    }

    /// Record that `name` was declared to be a type at the current depth.
    ///
    /// Duplicates are kept: a redeclaration is common (`class Widget;` then `class Widget { ... };`), and the
    /// scan looks from the end, so the earliest is simply never reached.
    pub fn declare(&mut self, name: &str) {
        // A guard against a pathological file turning the table into a memory problem. `MAX_DEPTH` and the
        // other budgets in this crate exist for the same reason: an editor parses whatever is in the buffer.
        const MAX_NAMES: usize = 4096;

        if self.bindings.len() >= MAX_NAMES {
            return;
        }

        self.bindings.push((name.into(), self.depth));
    }

    /// Is `name` a type name visible where the cursor is?
    ///
    /// Visible means declared at this depth or any shallower one, which is the approximation the module
    /// documentation describes. The **most recent** matching declaration decides, so a name redeclared as
    /// something else — which this table cannot represent, since it records nothing else — keeps its answer.
    pub fn is_a_type(&self, name: &str) -> bool {
        self.bindings
            .iter()
            .rev()
            .find(|(bound, _)| &**bound == name)
            .is_some_and(|(_, declared_at)| *declared_at <= self.depth)
    }

    /// How many names are recorded, for tests and for a consumer auditing the table.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Enter a braced body.
    ///
    /// Called for every `{ ... }`, including a function body and a class body, because what matters is only
    /// whether a *later* declaration at a shallower depth can see a name — and a name declared inside any brace
    /// is out of scope once the brace closes.
    pub fn enter_scope(&mut self) {
        self.depth += 1;
    }

    /// Leave a braced body.
    ///
    /// Saturating, because recovery can reach this without a matching enter: a missing `}` is ordinary in a file
    /// being typed, and a depth that went negative would make every name look out of scope.
    pub fn leave_scope(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// The current depth, for tests.
    pub fn depth(&self) -> usize {
        self.depth
    }
}
