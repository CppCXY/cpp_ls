//! A local table of the names a translation unit **`#define`s**.
//!
//! # Why the parser needs one
//!
//! A macro is expanded in translation phase 4, before any grammar runs, so a macro *invocation* is not a syntax
//! category the grammar can know: by the time the tokens are parsed, the macro is gone and what stands in its
//! place may be a specifier, a statement, a whole block, or nothing at all. This crate deliberately does not run
//! the preprocessor — see the module documentation of `grammar::cpp::stats` for what that costs and buys — and it
//! does not need to, because **the name is what the grammar was missing**:
//!
//! ```cpp
//! #define NUMBER_OPTION(op) if (auto value = ...; !value.empty()) { ... }
//!
//! NUMBER_OPTION(tab_width)      // no `;` — the macro's body is a whole statement
//! g(tab_width)                  // no `;` — a typo, and the only reading is an error
//! ```
//!
//! The two lines are the same tokens up to the name. A parser that guesses "this looks like a macro" from the
//! *spelling* catches both the real macro and every all-caps function; a parser that knows which names this file
//! defined catches the first and reports the second. That is the whole point of the table: it turns a convention
//! into evidence.
//!
//! # What it is and is not
//!
//! It is a *syntactic* table: a name is recorded because a `#define` directive wrote it, and nothing else. It
//! knows only this file — a macro from an included header (`TEST`, `Q_OBJECT`, `g_assert`) is **not** in it, and
//! that boundary is the same one [`crate::parser::TypeNames`] documents: no other file is read. Callers must
//! therefore treat a `false` as "this file does not say it is a macro" rather than "this is not a macro", and
//! keep whatever weaker evidence they have (the spelling convention) as a fallback.
//!
//! # Why there are no scopes
//!
//! `TypeNames` records a depth because a name declared inside a brace is not a type outside it. A macro is
//! **textual**: `#define` takes effect from the line it is written on to the end of the file, whatever braces
//! follow, and an `#undef` takes it away again. So the table has no depth — and the one thing it does record,
//! `#undef`, is recorded as a *removal*.

/// The names a translation unit's `#define` directives introduce.
///
/// See the module documentation for what is and is not recorded.
#[derive(Debug, Default, Clone)]
pub struct MacroNames {
    /// The names defined so far, in the order they were defined.
    ///
    /// A `Vec` rather than a set because `#undef` has to remove one, and because the scan looks from the end:
    /// a name defined twice is one name, and the most recent `#define` is the one that matters.
    defined: Vec<Box<str>>,
}

impl MacroNames {
    pub fn new() -> Self {
        MacroNames::default()
    }

    /// Record that `name` is defined as a macro.
    ///
    /// A name already in the table is not added twice: unlike a type name, a macro has no declaration to
    /// shadow — redefining one replaces it, and the answer to "is it a macro" is the same either way.
    pub fn define(&mut self, name: &str) {
        // A guard against a pathological file turning the table into a memory problem, for the same reason
        // `TypeNames` has one: an editor parses whatever is in the buffer.
        const MAX_NAMES: usize = 4096;

        if self.defined.len() >= MAX_NAMES || self.is_a_macro(name) {
            return;
        }

        self.defined.push(name.into());
    }

    /// Record that `name` is no longer defined — an `#undef`.
    pub fn undefine(&mut self, name: &str) {
        self.defined.retain(|defined| &**defined != name);
    }

    /// Is `name` one this file has defined?
    ///
    /// A `false` means "this file does not say so", not "this is not a macro": a macro from a header is
    /// invisible here. See the module documentation.
    pub fn is_a_macro(&self, name: &str) -> bool {
        self.defined.iter().any(|defined| &**defined == name)
    }

    /// How many names are recorded, for tests and for a consumer auditing the table.
    pub fn len(&self) -> usize {
        self.defined.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defined.is_empty()
    }

    /// The names defined, in definition order — for a consumer that wants to list them.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.defined.iter().map(|name| &**name)
    }
}

#[cfg(test)]
mod tests {
    use super::MacroNames;

    #[test]
    fn a_defined_name_is_a_macro_and_an_undefined_one_is_not() {
        let mut macros = MacroNames::new();
        assert!(!macros.is_a_macro("BOOL_OPTION"));

        macros.define("BOOL_OPTION");
        assert!(macros.is_a_macro("BOOL_OPTION"));
        assert!(!macros.is_a_macro("g"));
    }

    #[test]
    fn defining_twice_keeps_one_entry_and_undef_takes_it_away() {
        let mut macros = MacroNames::new();
        macros.define("IF_EXIST");
        macros.define("IF_EXIST");
        assert_eq!(macros.len(), 1);

        macros.undefine("IF_EXIST");
        assert!(!macros.is_a_macro("IF_EXIST"));
        assert_eq!(macros.len(), 0);
    }
}
