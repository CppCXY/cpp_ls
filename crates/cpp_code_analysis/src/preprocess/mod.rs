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

    /// **The spans of regions that were decided not to be compiled**, sorted by their start.
    ///
    /// Kept because one file produces **two** answers from these same directives — [`FilePreprocessing::macros`],
    /// which is a table, and the macro facts a summary carries — and a rule applied to one of them is not applied to
    /// the other. That is not hypothetical: `vcruntime.h` writes `_STL_LANG` once per branch of
    /// `#ifdef __cplusplus`, the table was taught to skip the dead ones, and the criterion did not move **at all**,
    /// because the environment a guard is evaluated against is built from the facts.
    ///
    /// Empty when no branch was decidable, which is the ordinary state: the two forms decided here are `#ifdef` and
    /// `#ifndef` against a name the compilation defines. Everything else is left as it was, deliberately — a region
    /// wrongly called dead loses a name that is really there.
    pub skipped_regions: Vec<(usize, usize)>,
}

impl FilePreprocessing {
    /// **Is the code at `offset` inside a branch that was decided not to be compiled?**
    ///
    /// The question [`FilePreprocessing::macros`] answers for itself while it is built, asked afterwards by a second
    /// consumer of the same directives. Two answers to one question is the shape of the defect this exists to close;
    /// one array and one method is the shape that stops it.
    pub fn is_skipped(&self, offset: usize) -> bool {
        self.skipped_regions
            .iter()
            .any(|(from, to)| *from <= offset && offset < *to)
    }

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
    preprocess_with(source, tokens, None)
}

/// [`preprocess`], with **what the compilation defines**.
///
/// The seed rather than the file's closure, and that is the whole point: the questions that decide a branch here —
/// `#ifdef __cplusplus`, `#ifndef _MSC_VER` — are about names the **compiler** predefines, and no file in the
/// closure knows them. Measured on `vcruntime.h`: its closure answered "does not know it" for `__cplusplus`, the
/// branch came out false, and `_STL_LANG` was recorded as `0L` while `_MSVC_LANG` was `202400L`.
///
/// [`preprocess_with`] takes the closure's own environment instead, for a caller that has one.
pub fn preprocess_with_a_seed(
    source: &str,
    tokens: &[CppTokenData],
    seed: &crate::Marked,
) -> FilePreprocessing {
    preprocess_using(source, tokens, &mut |name, offset| {
        crate::MacroValues::lookup(
            &crate::index::environment::MacrosHere::from_walk(seed, offset),
            name,
        ) != crate::Lookup::Undefined
    })
}

/// [`preprocess`], with **what the compilation already knows** so that a branch nobody could compile is not
/// recorded as a definition.
///
/// # The defect this exists for
///
/// A `#define` inside a region that is not compiled does not take effect — the module note says so — and the first
/// version honoured that by **recording every `#define` anyway** and leaving "which one is in force" to the guard on
/// the table. That is right for a consumer that wants to *see* a dead definition, and wrong for one asking what a
/// name means: the table answers "the last binding at or before this offset", and a dead binding is later than the
/// live one it shadows.
///
/// Measured, and it is the whole of a long report: `vcruntime.h` writes
///
/// ```cpp
/// #ifdef __cplusplus
///     #if defined(_MSVC_LANG) && _MSVC_LANG > __cplusplus
///         #define _STL_LANG _MSVC_LANG
///     #else
///         #define _STL_LANG __cplusplus
///     #endif
/// #else
///     #define _STL_LANG 0L          // <- the branch that was recorded, because it is the last
/// #endif
/// ```
///
/// so `_STL_LANG` came out `0L` while `__cplusplus` and `_MSVC_LANG` were both `202400L` — and the chain from there
/// is `_STL_LANG = 0` → `_HAS_CXX17 0` → `_HAS_CXX20 0` → the `#if _HAS_CXX20` around `#include <atomic>` in
/// `memory` decided inactive → `_Locked_pointer` invisible → ten `auto` declarations refused as `NotDeclaredHere`.
///
/// # What it asks, and what it leaves alone
///
/// Only a branch that **cannot have been taken**: `#ifdef NAME` where `NAME` is certainly undefined, `#ifndef NAME`
/// where it is certainly defined, and an `#else` whose `#if` was certainly taken. Everything else — every condition
/// that mentions a name the environment does not know, and every `#if` with operators, which this does not evaluate
/// — is recorded exactly as before. The direction matters: a definition wrongly kept costs a wrong answer in one
/// branch, and a definition wrongly dropped loses a name that is really there, so the rule fires only on an answer
/// the environment is **sure** of.
pub fn preprocess_with(
    source: &str,
    tokens: &[CppTokenData],
    environment: Option<&dyn cpp_parser::MacroFacts>,
) -> FilePreprocessing {
    preprocess_using(source, tokens, &mut |name, offset| {
        environment.is_some_and(|facts| facts.kind_of(name, offset).is_some())
    })
}

/// [`preprocess_with`] over the one question it asks: **is this name a macro at this offset?**
///
/// A closure rather than a trait object because the two callers answer it from different places — the seed for one,
/// the file's closure for the other — and neither answer is a `MacroValues` borrow that outlives the call.
fn preprocess_using(
    source: &str,
    tokens: &[CppTokenData],
    is_a_macro: &mut dyn FnMut(&str, usize) -> bool,
) -> FilePreprocessing {
    let directives = scan_directives(source, tokens);
    let mut macros = MacroTable::new();
    let mut stack = GuardStack::new();

    // **Which branch of the enclosing `#if`s is the one that gets compiled**, innermost last: `true` for a branch
    // that is taken, `false` for one that is not. A `#define` is recorded unless one of those is `false`.
    let mut live: Vec<bool> = Vec::new();

    // **The spans of the regions that were decided dead**, kept so that a consumer building a *second* answer from
    // the same directives — `macro_fact`, which turns them into facts — can ask the same question rather than
    // deriving its own. See [`FilePreprocessing::is_skipped`] for the defect that made this necessary.
    let mut skipped_regions: Vec<(usize, usize)> = Vec::new();
    // The offsets at which the current region's branches began, so a dead one can be closed when its `#endif`
    // arrives. Innermost last, like `live`.
    let mut openings: Vec<(usize, bool)> = Vec::new();

    for spanned in &directives {
        let kind = spanned.directive.kind();

        // A `#define` in a region that is not compiled never takes effect — but it is still recorded when nobody
        // can say it was skipped, because the region may be compiled under a different configuration, and because a
        // consumer asking "where is this macro defined" wants to see it either way.
        let skipped = live.iter().any(|taken| !taken);
        match &spanned.directive {
            Directive::Define(define) if !skipped => {
                if let Some(definition) = &define.macro_def {
                    macros.define(definition.clone());
                }
            }
            Directive::Undef { name: Some(name) } if !skipped => {
                macros.undefine(name, spanned.range.start_offset);
            }
            _ => {}
        }

        if let Some(branch) = branch_of(&spanned.directive, spanned.range) {
            // **The branch's verdict**, asked of the compilation rather than of the file's own table: these are
            // questions about names the compiler predefines (`__cplusplus`, `_MSC_VER`), and no directive above them
            // in this file has anything to say about those. `None` leaves the branch exactly as it was.
            let taken = crate::index::environment::a_branch_is_taken(
                is_a_macro,
                kind,
                &branch,
                spanned.range.start_offset,
            );

            // **The span of a branch that is dead is remembered**, from the directive that opens it to the one that
            // closes it. `Endif` closes whatever is innermost; `Elif`/`Else` close the branch they replace and open
            // their own.
            let here = spanned.range.start_offset;
            let settled = |taken: Option<bool>| taken.unwrap_or(true);

            match kind {
                DirectiveKind::If | DirectiveKind::Ifdef | DirectiveKind::Ifndef => {
                    let taken = settled(taken);
                    live.push(taken);
                    openings.push((here, taken));
                }
                DirectiveKind::Elif | DirectiveKind::Else => {
                    // **The branch being replaced is read before it is popped**, which the first version got
                    // backwards: it popped first and then read `live.last()`, which is the *enclosing* region
                    // rather than the branch before this one — so an `#else` inherited its grandparent's verdict.
                    let before = live.pop();
                    if let Some((opened, was_taken)) = openings.pop()
                        && !was_taken
                    {
                        skipped_regions.push((opened, here));
                    }
                    // **An `#else` is the opposite of the branch before it**, when that one was decided: it has no
                    // expression of its own, and `Branch::holds` answers `true` for it, which would keep a dead
                    // `#else`'s `#define` — the shape that made `_STL_LANG` `0L` in the first place. An `#elif` is a
                    // question of its own, and is left to the answer above.
                    let answer = match (kind, taken, before) {
                        (DirectiveKind::Else, _, Some(before)) => Some(!before),
                        _ => taken,
                    };
                    let taken = settled(answer);
                    live.push(taken);
                    openings.push((here, taken));
                }
                DirectiveKind::Endif => {
                    live.pop();
                    if let Some((opened, was_taken)) = openings.pop()
                        && !was_taken
                    {
                        // To the end of the `#endif` directive, so that a fact written on that line is covered too.
                        skipped_regions.push((opened, spanned.range.end_offset()));
                    }
                }
                _ => {}
            }

            stack.observe(kind, branch);
        }
    }

    skipped_regions.sort_unstable();
    FilePreprocessing {
        unclosed_guard: stack.guard(),
        directives,
        macros,
        skipped_regions,
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
