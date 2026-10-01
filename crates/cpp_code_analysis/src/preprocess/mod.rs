//! One traversal of a file that produces everything the layers above need.
//!
//! The three things this module builds are not independent, which is why they are built together
//! rather than by three passes:
//!
//! * the **macro table** changes at every `#define` and `#undef`, so "what does `FOO` mean here" is a
//!   question about position;
//! * a **guard**'s truth is decided by that table, at that position — the same `#if FOO` can be true
//!   early in a file and false later;
//! * a `#define` inside a region that is not compiled **does not take effect**, so the table depends on
//!   the guards.
//!
//! That last point is the circular one, and it is why a preprocessor is not a pure fold over the text.
//! The standard breaks the circle in the only place it can: a conditional directive is always obeyed —
//! `#if` decides which branch is live even inside `#if 0` — while a `#define` inside a skipped region is
//! ignored. So the traversal keeps *two* things at once: every `#define` is recorded (for a consumer
//! that wants to see it, and for the other branch of an `#if`), and which of them are in force is
//! answered by position through the guard on the table.

use cpp_parser::CppTokenData;

use crate::{
    directive::{Directive, DirectiveKind, SpannedDirective, scan_directives},
    guard::{Branch, Guard, GuardStack, Visibility},
    macros::{MacroBindings, MacroTable},
};

/// Everything one pass over a file produces.
#[derive(Debug, Clone, Default)]
pub struct FilePreprocessing {
    /// Every directive, in source order, with its conditions resolved.
    pub directives: Vec<SpannedDirective>,

    /// Macros, with each binding effective from the offset it was written at.
    pub macros: MacroTable,

    /// The guard in force at the end of the file.
    ///
    /// Empty for a well-formed file. Non-empty means an `#if` was never closed — the normal state of a
    /// file being edited, and the reason this is reported rather than treated as an error.
    pub unclosed_guard: Guard,
}

impl FilePreprocessing {
    /// The macros in force at `offset`.
    pub fn macros_at(&self, offset: usize) -> PositionalMacros<'_> {
        PositionalMacros {
            table: &self.macros,
            offset,
        }
    }

    /// Was the code at `offset` compiled?
    pub fn visibility_at(&self, offset: usize) -> Visibility {
        let guard = self.guard_at(offset);
        let macros = self.macros_at(offset);
        guard.visibility(&macros)
    }

    /// The guard in force at `offset`.
    ///
    /// Rebuilt from the directives rather than stored per byte: the number of conditionals is small
    /// and a byte-indexed table would dwarf the file it described.
    ///
    /// Only directives that have *finished* by `offset` count. A directive at the offset itself is not
    /// yet in force — `#if A` does not guard the line it is written on — which is why this compares
    /// against the directive's end rather than its start.
    pub fn guard_at(&self, offset: usize) -> Guard {
        let mut stack = GuardStack::new();

        for spanned in &self.directives {
            if spanned.range.end_offset() > offset {
                break;
            }
            if let Some(branch) = branch_of(&spanned.directive, spanned.range) {
                stack.observe(spanned.directive.kind(), branch);
            }
        }

        stack.guard()
    }
}

/// A macro table that answers as of one offset.
///
/// This is what [`crate::condition::MacroValues`] is implemented for, so a condition is evaluated
/// against the macros that were in force where it was written — not against the file's final table,
/// which is a different question with different answers.
///
/// The table is a `&dyn` because the same offset question is asked of a per-file table and of a whole walked
/// translation unit's view — see [`MacroBindings`], which is where the three sources of an answer are named.
pub struct PositionalMacros<'a> {
    table: &'a dyn MacroBindings,
    offset: usize,
}

impl PositionalMacros<'_> {
    /// Whether the compiler being read is MSVC's own: it predefines `_MSC_VER`, and clang-cl (which answers
    /// `__has_cpp_attribute` as clang does) also predefines `__clang__`.
    fn is_msvc(&self) -> bool {
        self.table.definition_at("_MSC_VER", self.offset).is_some()
            && self.table.definition_at("__clang__", self.offset).is_none()
    }
}

impl crate::condition::MacroValues for PositionalMacros<'_> {
    fn lookup(&self, name: &str) -> crate::condition::Lookup<'_> {
        use crate::condition::Lookup;

        self.table
            .definition_at(name, self.offset)
            .map_or(Lookup::Undefined, Lookup::Defined)
    }

    /// **Forwarded, with the offset this wrapper exists to add.**
    ///
    /// The cooks all evaluate through a [`PositionalMacros`], so an operator answered by the table underneath is
    /// answered here or nowhere: `__has_include` would be `Unknown` in every real cook while its implementation sat
    /// one layer down, unreachable. The wrapper's whole job is to add the offset.
    ///
    /// # The offset is not decoration
    ///
    /// An earlier version of this forwarded the operator with **no** position, and for `__has_cpp_attribute` that was
    /// the whole bug: the answer depends on whether the bare name is a macro *at that point*, and the walking table
    /// answers a question without a position from the **end of the file** — where `xtr1common:22`'s `#undef msvc`
    /// has already happened, so every `msvc::` attribute read as unsupported and
    /// `[[msvc::no_specializations(...)]]` was missing from the stream while `cl.exe` emitted it 17 times.
    fn builtin_operator(&self, name: &str, operand: &str) -> Option<crate::condition::Value> {
        if name == "__has_cpp_attribute" {
            let msvc = self.is_msvc();
            let answer = crate::preprocess::cooked::attribute_support_in(operand, msvc);
            if std::env::var_os("CPPLS_TRACE_ATTR").is_some() {
                eprintln!("cppls-trace: __has_cpp_attribute({operand:?}) at {} -> {answer:?}", self.offset);
            }
            return answer.map(crate::condition::Value::Known);
        }

        self.table.builtin_operator(name, operand)
    }
}

/// Read a file's directives, macros, and conditionals in one pass.
///
/// **The input is the token stream**, not the tree: a directive is a line, and where a line begins is a
/// fact about tokens. The tree is built *from* this same stream (see `cpp_parser::lex`), so the two cannot
/// disagree about where a token is — there is one stream and both read it.
pub fn preprocess(source: &str, tokens: &[CppTokenData]) -> FilePreprocessing {
    let directives = scan_directives(source, tokens);
    let mut macros = MacroTable::new();
    let mut stack = GuardStack::new();

    for spanned in &directives {
        let kind = spanned.directive.kind();

        // A `#define` in a region that is not compiled never takes effect — but it is still recorded,
        // because the region may be compiled under a different configuration, and because a consumer
        // asking "where is this macro defined" wants to see it either way.
        match &spanned.directive {
            Directive::Define(define) => {
                if let Some(definition) = &define.macro_def {
                    macros.define(definition.clone());
                }
            }
            Directive::Undef { name: Some(name) } => {
                macros.undefine(name, spanned.range.start_offset);
            }
            _ => {}
        }

        if let Some(branch) = branch_of(&spanned.directive, spanned.range) {
            stack.observe(kind, branch);
        }
    }

    FilePreprocessing {
        unclosed_guard: stack.guard(),
        directives,
        macros,
    }
}

/// The conditional branch a directive writes, if it writes one.
fn branch_of(directive: &Directive, range: cpp_parser::SourceRange) -> Option<Branch> {
    let kind = directive.kind();
    if !(kind.opens_a_condition() || kind.closes_a_condition()) {
        return None;
    }

    let (tokens, name) = match directive {
        Directive::Conditional { condition, .. } => (condition.clone(), None),
        Directive::Ifdef { name, .. } => (Vec::new(), Some(name.clone())),
        _ => (Vec::new(), None),
    };

    Some(Branch {
        kind,
        tokens,
        name,
        range,
    })
}

/// Every macro name a file might want to know about, whether or not it is in force.
///
/// For completion: a name defined in a branch that is not compiled is still worth offering, because
/// the user may be about to change the configuration — or may be writing the other branch.
pub fn candidate_macro_names(preprocessing: &FilePreprocessing) -> Vec<&str> {
    let mut names: Vec<&str> = preprocessing
        .directives
        .iter()
        .filter_map(|spanned| spanned.directive.defines())
        .map(|definition| &*definition.name)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Is this directive one that opens a region left unclosed at the end of the file?
pub fn opens_unclosed_region(directive: &Directive) -> bool {
    matches!(
        directive.kind(),
        DirectiveKind::If | DirectiveKind::Ifdef | DirectiveKind::Ifndef
    )
}

// The pieces this entry point is built from:
//
//   the token stream              -> typed Directive values        (directive)
//   #define                     -> MacroDef, token sequence      (macros)
//   #if / #elif                 -> Guard conditions over macros  (condition, guard, guards)
//   a macro call                -> a shadow token stream, every
//                                  token carrying its origin      (expand)
//
// Nothing here is re-lexed from the source: the tokens come from `cpp_parser::lex`, which is the same
// function the parser reads its own stream from, so the two layers cannot drift apart.
pub mod condition;
pub mod cooked;
pub mod directive;
pub mod expand;
pub mod guard;
pub mod guards;
pub mod macros;
