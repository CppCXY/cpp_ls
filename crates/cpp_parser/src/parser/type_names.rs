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
//! # Scopes are a stack, not a depth
//!
//! Every `{ … }` pushes a scope and its closing brace pops it, so a name is visible exactly where a reader would
//! expect: the file's own declarations everywhere below them, a class's or function's own inside that body.
//!
//! This used to be a single map from name to *the depth it was declared at*, with visibility computed as
//! `declared_at <= depth`. That is not a scope rule, and the difference is not academic: two sibling functions
//! share a depth, so a `typedef` written in the first was still a type name inside the second, and
//!
//! ```cpp
//! void first()  { typedef long Leaked; }
//! void second() { auto Leaked = [] {}; Leaked(); }
//! ```
//!
//! came out as a **declaration** of `Leaked` with a parameter list — no `CallExpr`, no diagnostic, a binding that
//! does not exist. The other half was lost in the same way: a name declared in an inner scope *replaced* the
//! outer entry, so the outer type never came back when the inner scope closed. Both are the review's P1-1
//! (`docs/worklog.md`), and both are gone with a stack of maps.
//!
//! # What is still not modelled
//!
//! A **variable** that hides a type name is not recorded: the table can say "type" or nothing, and nothing loses
//! to the outer type when the inner binding shadows it:
//!
//! ```cpp
//! struct Widget {};
//! void f() { int Widget = 0; Widget = 1; }   // an assignment, read as a declaration attempt
//! ```
//!
//! Recording value bindings means recording **every declarator's name**, which is a much larger claim on the
//! parser than the narrow one this table makes. It is the review's P1-1 step 2 and deliberately left out here.

/// The names a translation unit's declarations introduce as types.
///
/// See the module documentation for what is and is not recorded.
#[derive(Debug, Default, Clone)]
pub struct TypeNames {
    /// One map per open brace, innermost last; the **first** entry is the file's own scope.
    ///
    /// Never empty: `new` starts with the file scope so `declare` has somewhere to write before any brace.
    scopes: Vec<std::collections::HashMap<Box<str>, ()>>,
    /// How many names are recorded, across every scope — what the cap below bounds.
    ///
    /// Insertions rather than distinct names: a name declared in two scopes has two entries, which is what the
    /// memory is actually spent on.
    names: usize,
}

impl TypeNames {
    pub fn new() -> Self {
        TypeNames {
            scopes: vec![std::collections::HashMap::new()],
            names: 0,
        }
    }

    /// Record that `name` was declared to be a type in the scope the cursor is in.
    ///
    /// Re-recording a name in the same scope replaces its entry, which is what a redeclaration means —
    /// `class Widget;` followed by `class Widget { … };` leaves one binding either way.
    pub fn declare(&mut self, name: &str) {
        // A guard against a pathological file turning the table into a memory problem. `MAX_DEPTH` and the
        // other budgets in this crate exist for the same reason: an editor parses whatever is in the buffer.
        const MAX_NAMES: usize = 4096;

        let scope = self
            .scopes
            .last_mut()
            .expect("`TypeNames::new` starts with the file scope");
        if !scope.contains_key(name) {
            if self.names >= MAX_NAMES {
                return;
            }
            self.names += 1;
        }
        scope.insert(name.into(), ());
    }

    /// Is `name` a type name visible where the cursor is?
    ///
    /// Visible means declared in this scope or in any enclosing one — and the **innermost** binding is what
    /// answers, so a class that declares a member type of the same name hides the outer one inside its body.
    pub fn is_a_type(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .any(|scope| scope.contains_key(name))
    }

    /// How many name bindings are recorded, for tests and for a consumer auditing the table.
    ///
    /// Insertions per scope, so a name declared in two scopes counts twice; the cap above bounds this.
    pub fn len(&self) -> usize {
        self.names
    }

    pub fn is_empty(&self) -> bool {
        self.names == 0
    }

    /// Enter a braced body.
    ///
    /// Called for every `{ ... }`, including a function body and a class body: a name declared inside a brace is
    /// out of scope once that brace closes, and a name declared outside stays visible inside.
    pub fn enter_scope(&mut self) {
        self.scopes.push(std::collections::HashMap::new());
    }

    /// Leave a braced body, dropping everything declared in it.
    ///
    /// The file's own scope is never popped, because recovery can reach this without a matching `enter`: a
    /// missing `}` is ordinary in a file being typed, and a parser with no scope at all would make every name
    /// look out of scope.
    pub fn leave_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.names -= self.scopes.pop().map_or(0, |scope| scope.len());
        }
    }

    /// The current depth — how many braced bodies are open, `0` at file scope.
    ///
    /// What the parser asks to tell a statement's scope from the file's: see
    /// [`CppParser::is_at_file_scope`](crate::parser::CppParser::is_at_file_scope).
    pub fn depth(&self) -> usize {
        self.scopes.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::TypeNames;

    /// **A name declared in one sibling scope is not visible in the next** — the review's P1-1 reproduction.
    #[test]
    fn a_name_declared_in_one_brace_is_not_a_type_in_a_sibling() {
        let mut names = TypeNames::new();
        names.enter_scope();
        names.declare("Leaked");
        assert!(
            names.is_a_type("Leaked"),
            "visible in the scope it was written in"
        );
        names.leave_scope();
        assert!(
            !names.is_a_type("Leaked"),
            "and not in the sibling after it"
        );
    }

    /// **An inner declaration shadows the outer one and gives it back.** The old flat map kept only the newest
    /// binding, so the outer type was lost for the rest of the file.
    #[test]
    fn an_inner_declaration_restores_the_outer_one_when_its_scope_closes() {
        let mut names = TypeNames::new();
        names.declare("Widget");
        names.enter_scope();
        names.declare("Other");
        assert!(
            names.is_a_type("Widget"),
            "the outer name stays visible inside"
        );
        assert!(names.is_a_type("Other"));
        names.leave_scope();
        assert!(names.is_a_type("Widget"), "and is still there afterwards");
        assert!(
            !names.is_a_type("Other"),
            "the inner one is gone with its scope"
        );
    }

    /// The file's own scope is never popped, even when recovery leaves more braces than it opened.
    #[test]
    fn leaving_more_scopes_than_were_entered_keeps_the_file_scope() {
        let mut names = TypeNames::new();
        names.declare("Widget");
        names.leave_scope();
        names.leave_scope();
        assert_eq!(names.depth(), 0);
        assert!(names.is_a_type("Widget"));
    }

    /// The depth is still the number of open braces, which is what the parser's scope questions read.
    #[test]
    fn the_depth_counts_open_braces() {
        let mut names = TypeNames::new();
        assert_eq!(names.depth(), 0);
        names.enter_scope();
        assert_eq!(names.depth(), 1);
        names.enter_scope();
        assert_eq!(names.depth(), 2);
        names.leave_scope();
        assert_eq!(names.depth(), 1);
    }
}
