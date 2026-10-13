//! Guards: which conditional regions a piece of code sits inside, and whether it is being compiled.
//!
//! # Why a guard is a list, not a boolean
//!
//! The decision this layer supports is *not* "take this branch and drop the other". A compiler does
//! that; an editor cannot, because both branches are code somebody is writing and both have to stay in
//! the tree, in the index, and in the file's outline. What the editor needs to know is narrower:
//!
//! * Is this declaration visible in the configuration we are analysing under?
//! * If not, is it because the configuration says so, or because we cannot tell?
//!
//! So a guard is the list of conditions a region sits inside, each carrying its own truth value, and
//! the answer is computed from the list rather than from a single flag. Code at file scope has an empty
//! guard, which is `Active` — the common case, and it stays cheap.
//!
//! # Why the list is nested and not flat
//!
//! An `#elif` or `#else` is an alternative to the branches *at its own level*, and only to those. In
//!
//! ```text
//! #if A
//! #else          <- taken when A is false
//! #  if B
//! #  else        <- taken when B is false
//! #  endif
//! #endif
//! ```
//!
//! a flat list of `[A, !A, B, !B]` cannot say which `#else` pairs with which `#if`, and the second
//! `#else` would be compared against `A` as well. So a guard is a list of *regions*, and each region
//! holds the chain of branches written for it.
//!
//! # `Unknown` is the point
//!
//! ```text
//! #if defined(_WIN32)
//! void win_only();
//! #else
//! void posix_only();
//! #endif
//! ```
//!
//! On a machine where the configuration does not mention `_WIN32`, the honest answers are "the first is
//! not compiled" and "the second is" — *if* the configuration is complete. If the analysis has no
//! configuration at all, both are `Unknown`, and a consumer that renders `Unknown` as "still visible,
//! but do not diagnose inside it" gets the editor behaviour people expect: completion offers both, and
//! neither is called an error.

use cpp_parser::SourceRange;

use crate::{
    condition::{MacroValues, evaluate_recording},
    directive::DirectiveKind,
    token::Token,
};

/// One branch of a conditional region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// Which directive wrote it: `#if`, `#ifdef`, `#ifndef`, `#elif`, `#else`.
    pub kind: DirectiveKind,
    /// The expression's tokens, for the `#if` and `#elif` forms.
    pub tokens: Vec<Token>,
    /// The name tested, for `#ifdef` and `#ifndef`.
    pub name: Option<Box<str>>,
    /// Where the branch was written.
    pub range: SourceRange,
    /// **The parsed expression, when macro replacement cannot change it.** `None` means either "not decided
    /// yet" or "this one has to be re-read every time" — the flag below tells the two apart.
    ///
    /// `evaluate` expands the tokens and parses the result **on every call**, and a branch is evaluated once per
    /// guard, per include, per translation unit that reaches the file it is written in — millions of times for a
    /// real project's index. But `expand_condition` only does anything for a **function-like macro call**
    /// (`#if WINAPI_FAMILY_PARTITION(WINAPI_PARTITION_APP)`); an object-like name is not expanded there at all, it
    /// is looked up during `eval` by `ConditionExpr::Identifier`. So for a condition with no function-like call the
    /// expansion is the identity, the parse depends on nothing, and re-reading it millions of times is pure waste.
    ///
    /// `PartialEq`/`Eq` compare the two `OnceLock`s as well, which they can because `ConditionExpr` and `bool` both
    /// have it — and that is the honest comparison: two branches that have computed different things about the same
    /// tokens *are* different values, and the only ways to get here are `Clone` (which copies them) and the four
    /// construction sites (which leave them empty).
    parsed: std::sync::OnceLock<Option<crate::preprocess::condition::ConditionExpr>>,
    /// `true` once this branch is known to be macro-independent — either because the expansion came back
    /// unchanged, or because parsing it failed, which no macro state can repair.
    fixed: std::sync::OnceLock<bool>,
    /// **The whole answer, for a condition that is one number.** `#if 0` and `#if 1` decide themselves and are
    /// common in real headers, so they are answered without touching the macro table at all.
    ///
    /// A separate cache from [`Branch::parsed`] because it holds a different kind of thing: that one is an
    /// expression that still has to be evaluated against a table, this one is the answer.
    constant: std::sync::OnceLock<Option<bool>>,
}

impl Branch {
    /// One branch, with its two caches empty.
    ///
    /// A constructor rather than struct literals at the four call sites, because the caches are private and must
    /// always start empty: carrying a parse across from a *different* token list would be a wrong answer that nothing
    /// downstream could detect.
    pub fn new(
        kind: DirectiveKind,
        tokens: Vec<Token>,
        name: Option<Box<str>>,
        range: SourceRange,
    ) -> Branch {
        Branch {
            kind,
            tokens,
            name,
            range,
            parsed: std::sync::OnceLock::new(),
            fixed: std::sync::OnceLock::new(),
            constant: std::sync::OnceLock::new(),
        }
    }

    /// Does this branch's own expression hold?
    ///
    /// `None` when it cannot be decided. An `#else` has no expression, so it always "holds" — whether
    /// the region as a whole takes it is a question about the branches before it.
    ///
    /// Evaluating on demand rather than caching a value is deliberate: whether a condition holds depends
    /// on the macro table *at that point in the file*, and a guard cloned onto a declaration carries no
    /// position of its own. The caller has the position; this does not need it.
    pub fn holds(&self, macros: &impl MacroValues) -> Option<bool> {
        // **A trace on one condition, for the walk's own evaluation path.** The cook and the walk decide the same
        // guards through different code, and when they disagree the disagreement is invisible from outside: this
        // prints what *this* path decided, for a condition whose text mentions the name the caller asked about.
        //
        // The variable is read once — see `crate::probes`. This is called for every branch of every region of every
        // condition of every guard, tens of millions of times in one index, and reading the environment here was a
        // lock of the process per branch: **40 s** of a real project's index, measured.
        if let Some(wanted) = crate::probes::guard_name() {
            let mentions = self.tokens.iter().any(|token| token.text().contains(wanted));
            if mentions {
                let answer = match self.kind {
                    DirectiveKind::If | DirectiveKind::Elif => {
                        evaluate_recording(&self.tokens, macros).0.is_true()
                    }
                    DirectiveKind::Ifdef => is_defined(self.name.as_deref(), macros),
                    DirectiveKind::Ifndef => {
                        is_defined(self.name.as_deref(), macros).map(|defined| !defined)
                    }
                    DirectiveKind::Else => Some(true),
                    _ => None,
                };
                println!(
                    "cppls-guard: {:?} mentions {wanted} -> {answer:?}",
                    self.tokens
                        .iter()
                        .map(|token| token.text())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                return answer;
            }
        }

        match self.kind {
            DirectiveKind::If | DirectiveKind::Elif => self.an_expression_holds(macros),
            DirectiveKind::Ifdef => is_defined(self.name.as_deref(), macros),
            DirectiveKind::Ifndef => is_defined(self.name.as_deref(), macros).map(|defined| !defined),
            DirectiveKind::Else => Some(true),
            _ => None,
        }
    }

    /// This branch's `#if`/`#elif` expression, **read once when the answer cannot depend on the macros**.
    ///
    /// Two cases, and only two, because those are the two that can be *proved* cheaply:
    ///
    /// * **A constant.** `#if 0` and `#if 1` decide themselves, and so does any condition whose tokens are one
    ///   number. They are extremely common in real headers — every "not yet" and every "always" — and they need no
    ///   macro state at all, so the answer is computed once and kept.
    /// * **A parse that macro replacement cannot change.** Reserved for the case where the expansion came back
    ///   identical, which is rare for the reason below.
    ///
    /// **What this deliberately does not do.** An earlier attempt kept the parse whenever the expansion was the
    /// identity, and measured *slower*: object-like macros are replaced too (`#if _MSC_VER >= 1930` becomes
    /// `#if 1930 >= 1930`), so the identity case is rare, the cache almost never fills, and the test cost a flag on
    /// every call. The tempting fix — "keep the parse when the condition has no function-like call" — is **wrong**,
    /// and the reason is in `expand_condition`: it skips the operand of `defined`, so `defined(FOO)` must not be
    /// expanded, but a *bare* `FOO` must be, and the two are the same token followed by the same `(`.
    fn an_expression_holds(&self, macros: &impl MacroValues) -> Option<bool> {
        if let Some(held) = self.constant.get() {
            return *held;
        }
        if *self.fixed.get().unwrap_or(&false) {
            // Sound because `fixed` is only set together with `parsed`, and only when the expansion was the
            // identity — so re-parsing would yield this same expression.
            return match self.parsed.get().and_then(Option::as_ref) {
                Some(expr) => crate::preprocess::condition::eval(expr, macros).is_true(),
                // Stored as `None`: the tokens do not parse, and no macro state makes them parse. `evaluate`
                // answers `Unknown` for that, which is `None` here.
                None => None,
            };
        }

        // **One number decides itself.** Checked before anything is expanded, so a constant condition never touches
        // the macro table — which is what makes it worth having as its own case rather than folded into the other.
        if let Some(held) = a_constant_condition(&self.tokens) {
            let _ = self.constant.set(Some(held));
            return Some(held);
        }

        let (value, expression) = evaluate_recording(&self.tokens, macros);
        if let Some(expression) = expression {
            let _ = self.parsed.set(Some(expression));
            let _ = self.fixed.set(true);
        }
        value.is_true()
    }
}

/// **The answer to a condition that is one number**, or `None` when it is anything else.
///
/// `#if 0` is the idiom for "not yet" and `#if 1` for "always", and neither asks about a macro. A condition of
/// exactly one numeric token is `Known` whatever the environment says, so the whole answer is its truth value — not
/// merely a parse that can be kept — and it holds for the life of the process.
///
/// Deliberately **not** generalised to "a constant expression" (`#if 0 && FOO`): that needs the parse this exists to
/// avoid, and short-circuiting is the compiler's job, not this function's.
fn a_constant_condition(tokens: &[Token]) -> Option<bool> {
    let mut significant = tokens
        .iter()
        .filter(|token| !crate::token::is_trivia(token.kind));
    let only = significant.next()?;
    if significant.next().is_some() {
        return None;
    }
    // A condition cannot be a float, and C++ has no `true`/`false` keyword in the preprocessor, so a numeric literal
    // is the whole of what can be here.
    let value = only.text().parse::<i128>().ok()?;
    Some(value != 0)
}

/// Is the name a macro, according to a table that may not be able to say?
///
/// `#ifdef NAME` asks exactly `defined(NAME)`, so it gives the same answer for the same reason: a name the
/// table cannot speak about is **not** "not defined" — see [`crate::condition::Lookup`]. `Some(false)` is
/// the answer only for [`Lookup::Undefined`], which is the one table state that claims completeness, and a
/// directive with no name at all is `None` for the same reason: there is nothing to decide.
fn is_defined(name: Option<&str>, macros: &impl MacroValues) -> Option<bool> {
    name.and_then(|name| {
        // **A builtin operator is defined, and the compiler is the authority for that.**
        //
        // `defined(__has_cpp_attribute)` and `#ifndef __has_cpp_attribute` are **true** on clang, gcc and MSVC alike:
        // these operators are predefined macros of the implementation, not ordinary names, which is exactly how a
        // header tells whether the implementation has the operator at all. clang has a formal list of them
        // (`__has_feature`, `__has_builtin`, `__has_attribute`, `__has_cpp_attribute`, …).
        //
        // Answering from the macro table instead was the head of a long chain: `yvals_core.h:655` writes
        //
        // ```cpp
        // #ifndef __has_cpp_attribute
        // #define _HAS_MSVC_ATTRIBUTE(x) 0        // ← we took this
        // #else
        // #define _HAS_MSVC_ATTRIBUTE(x) __has_cpp_attribute(msvc::x)
        // #endif
        // ```
        //
        // so every `_HAS_MSVC_ATTRIBUTE` below it became the literal `0`, and the table of MSVC attributes in
        // [`crate::preprocess::cooked::supported_attribute`] could never be reached — `_NO_SPECIALIZATIONS_OF_TYPE_TRAITS`
        // expanded to nothing and `[[msvc::no_specializations(...)]]` was missing from the stream at
        // `xtr1common:27`.
        if crate::preprocess::condition::is_a_builtin_operator(name) {
            return Some(true);
        }

        macros.lookup(name).is_defined()
    })
}

/// One conditional region: the chain of branches written for a single `#if`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The branches in source order; the first is always the opener.
    pub branches: Vec<Branch>,
    /// Which branch is in force at the position being asked about.
    pub active_branch: usize,
}

impl Region {
    /// Is the branch in force at this position the one that gets compiled?
    ///
    /// The rule is the standard's, in order: the first branch whose own expression holds is taken; a
    /// branch whose expression does not hold is not; and if any earlier branch is undecidable, then
    /// whether *this* branch is taken depends on it.
    pub fn visibility(&self, macros: &impl MacroValues) -> Option<bool> {
        // Whether some branch before the active one is undecidable. If it is, then even a definite
        // "yes" from the active branch does not settle the region: the undecidable one might have been
        // taken first. `#if X` / `#elif 1` is the case — `1` holds, and yet whether the region is taken
        // depends entirely on `X`.
        let mut unknown_before = false;

        for (index, branch) in self.branches.iter().enumerate() {
            let holds = branch.holds(macros);

            if index < self.active_branch {
                match holds {
                    // An earlier branch is taken, so this one is not.
                    Some(true) => return Some(false),
                    Some(false) => {}
                    None => unknown_before = true,
                }
                continue;
            }

            if index > self.active_branch {
                break;
            }

            return match (holds, unknown_before) {
                (None, _) => None,
                // The active branch does not hold, and an earlier branch might have been taken
                // instead. Either way this branch is not the one compiled, so the answer is a
                // definite "no": its own expression already rules it out.
                (Some(false), _) => Some(false),
                // The active branch holds, but an earlier one might have been taken instead — so
                // whether *this* region is taken depends on that earlier one.
                (Some(true), true) => None,
                (Some(true), false) => Some(true),
            };
        }

        // The active index is past the end, which should not happen. "Cannot tell" is the safe answer
        // rather than a panic in an editor.
        None
    }
}

/// Whether the code inside a guard is being compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Every condition holds, so this code is compiled. Code at file scope is always this.
    Active,
    /// Some condition does not hold, so this code is not compiled under the configuration in hand. It
    /// stays in the tree and in the index; a consumer greys it out rather than dropping it.
    Inactive,
    /// At least one condition cannot be decided, and none of the ones that *can* be decided rules the
    /// region out.
    Unknown,
}

impl Visibility {
    /// Should a consumer treat this code as reachable?
    ///
    /// `Active` and `Unknown` both answer yes: code whose visibility is unknown is code that might be
    /// compiled, and hiding it would be the wrong guess. This is the predicate completion should use.
    pub fn is_reachable(self) -> bool {
        !matches!(self, Visibility::Inactive)
    }

    /// Should a consumer report diagnostics inside this code?
    ///
    /// Only `Active`. Reporting an error in a branch that is not compiled is a false positive, and an
    /// editor-facing tool that reports false positives gets switched off.
    pub fn is_diagnosable(self) -> bool {
        matches!(self, Visibility::Active)
    }
}

/// The conditional regions an item sits inside, outermost first.
///
/// Cloned freely rather than shared: a guard is a handful of small values, and a file has one per
/// `#if`, so the copying is bounded by the number of conditionals rather than by the number of
/// declarations that consult it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Guard {
    regions: Vec<Region>,
}

impl Guard {
    /// The guard of code at file scope: nothing to satisfy.
    pub fn empty() -> Self {
        Guard {
            regions: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// How deeply nested the conditionals are.
    pub fn depth(&self) -> usize {
        self.regions.len()
    }

    /// The branch in force at the innermost level, if the code sits inside any conditional at all.
    ///
    /// This is what a UI asks to draw the `#if` a line belongs to.
    pub fn innermost_branch(&self) -> Option<&Branch> {
        let region = self.regions.last()?;
        region.branches.get(region.active_branch)
    }

    /// Report whether this code is compiled under `macros`.
    ///
    /// A region is `Inactive` as soon as one region is known not to be taken, and `Unknown` when none
    /// is ruled out but one cannot be decided. That ordering matters: code inside `#if 0` is inactive
    /// even if an inner `#if` is undecidable, because the outer one already settled it.
    pub fn visibility(&self, macros: &impl MacroValues) -> Visibility {
        let mut unknown = false;

        for region in &self.regions {
            match region.visibility(macros) {
                Some(true) => {}
                Some(false) => return Visibility::Inactive,
                None => unknown = true,
            }
        }

        if unknown {
            Visibility::Unknown
        } else {
            Visibility::Active
        }
    }
}

/// Builds guards by walking a file's directives in order.
///
/// Exists as a type rather than as a function over a slice because the interesting state is the stack,
/// and because a caller may want to interleave its own walk — asking "what guard is in force here?" as
/// it moves through the file.
#[derive(Debug, Clone, Default)]
pub struct GuardStack {
    stack: Vec<Region>,
}

impl GuardStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// The guard in force, as a value a consumer can attach to a declaration.
    pub fn guard(&self) -> Guard {
        Guard {
            regions: self.stack.clone(),
        }
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Account for a conditional directive.
    ///
    /// Directives that are not conditional are ignored, so a caller can feed a whole file through
    /// without deciding what matters.
    pub fn observe(&mut self, kind: DirectiveKind, branch: Branch) {
        match kind {
            DirectiveKind::If | DirectiveKind::Ifdef | DirectiveKind::Ifndef => {
                self.stack.push(Region {
                    branches: vec![branch],
                    active_branch: 0,
                });
            }
            DirectiveKind::Elif | DirectiveKind::Else => {
                if let Some(region) = self.stack.last_mut() {
                    // A new branch becomes the one in force for everything after it. Whether it is
                    // *taken* is a question for `visibility`; "which branch are we lexically inside"
                    // is this, and the two are different questions.
                    region.branches.push(branch);
                    region.active_branch = region.branches.len() - 1;
                }
            }
            DirectiveKind::Endif => {
                self.stack.pop();
            }
            _ => {}
        }
    }
}
