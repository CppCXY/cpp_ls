//! **The macro environment a question is asked against, as the preprocessor itself can see one.**
//!
//! The walk in [`crate::preprocess`] does not know what a file *means*; it knows which directives it read and which
//! names the compilation had in force at a given offset. This module is the one decision that needs both — "is this
//! branch of an `#if` the one that gets compiled" — kept here rather than in the analysis so that the analysis may
//! depend on it and not the other way round.

use crate::DirectiveKind;
use crate::preprocess;
/// **Is this branch the one that gets compiled?** Asked of the compilation's own knowledge, before a file is read.
///
/// `None` when nobody can say — no environment, or a condition whose expression needs a body — which leaves the
/// branch exactly as the walk treated it before this existed. The direction is deliberate: a branch wrongly called
/// *dead* loses a `#define` that is really there, while one wrongly called *live* keeps a definition that shadows a
/// better one. Only an answer the environment is **sure** of is turned into a decision.
///
/// Written for `vcruntime.h`:
///
/// ```cpp
/// #ifdef __cplusplus
///     #define _STL_LANG _MSVC_LANG
/// #else
///     #define _STL_LANG 0L          // <- recorded as the definition, because it is the last one
/// #endif
/// ```
///
/// `__cplusplus` is the compiler's own name and is in the environment, so this answers `true` for the first branch
/// and `false` for the `#else` — which is what stops `0L` from shadowing the real value.
///
/// **The two forms it decides, and nothing else.** `#ifdef NAME` and `#ifndef NAME` are questions about definedness,
/// which the environment answers immediately; everything else — `#if X > 2`, `defined X`, an `#elif` whose verdict
/// depends on the branches before it — needs a body or an order, and is left alone. That is not a limitation of this
/// function but of the evidence: a preprocessor that guessed at `#if` would silently delete code a person wrote.
pub fn a_branch_is_taken(
    is_a_macro: &mut dyn FnMut(&str, usize) -> bool,
    kind: DirectiveKind,
    branch: &preprocess::guard::Branch,
    offset: usize,
) -> Option<bool> {
    match kind {
        DirectiveKind::Ifdef => {
            let name = branch.name.as_deref()?;
            Some(is_a_macro(name, offset))
        }
        DirectiveKind::Ifndef => {
            let name = branch.name.as_deref()?;
            Some(!is_a_macro(name, offset))
        }
        _ => None,
    }
}
