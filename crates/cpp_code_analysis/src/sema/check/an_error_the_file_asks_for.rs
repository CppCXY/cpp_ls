//! **`#error` — the file asking the compilation to stop.**
//!
//! ```cpp
//! #if !defined(_WIN32) && !defined(__linux__)
//! #error this build is not supported on this platform
//! #endif
//! ```
//!
//! Every compiler reports this, and it is the one diagnostic a file writes **for itself**: the message is the
//! author's, and it is usually the most useful sentence anyone will read about the failure. Measured before this
//! check existed: a live `#error` produced *nothing* — the reading reported zero errors, because the directive
//! is not a syntax error and nothing else had a channel for it.
//!
//! # The question this check is really asking
//!
//! Not "is there an `#error` in this file" — that is a text search. The question is **is the branch it sits in
//! the branch that is compiled**. `#error` in the arm of an `#if` that was not taken is not a diagnostic, it is
//! the normal way a header explains which platforms it supports; reporting it would fire on every portable
//! header in the closure.
//!
//! That question is answered by the guard layer, which is already three-valued and already documented for
//! exactly this caller — [`Visibility::is_diagnosable`]: *"Should a consumer report diagnostics inside this
//! code? Only `Active`. Reporting an error in a branch that is not compiled is a false positive, and an
//! editor-facing tool that reports false positives gets switched off."*
//!
//! # Why no fact is stored for this, and what that costs
//!
//! `#error` is not a declaration, a macro or an include, so a summary does not record it — and the first plan
//! for this check was to add one. It is not needed: [`SummaryGuards`](crate::SummaryGuards) already stores the
//! conditional structure, and [`GuardBranch::body`](crate::GuardBranch) is a **range**, so the region containing
//! a directive is a containment test against facts that are already on disk.
//!
//! What the summary stores is the *conditionals*; what it does not store is where this file's `#error`s are.
//! The directives are read from the file's own tree instead, which is the same tree the rest of the analysis
//! read — not a second scanner over the text, which is the mistake this codebase warns about in
//! [`MacroFact::body_range`]'s own documentation.
//!
//! # The gate, and why it is the first thing in the function
//!
//! A **cooked** reading has no conditionals: `FileSummary::map_into_the_file` says so — *"A rendering has none
//! of them"*. So a file read as a compiler reads it hands this check a summary that describes no `#if` at all,
//! and a `#error` written inside `#if 0` would then find no region containing it. The tempting reading of "no
//! region contains this" is "it is at file scope, so it is compiled" — which would report a false positive on
//! exactly the construct the check exists to stay quiet about.
//!
//! So the two cases are told apart before anything is reported: if the file's tree has conditional directives
//! and the summary describes none, this check has nothing it can stand behind and says nothing.
//! [`visibility_at`] answers `Unknown` for a region a summary does not describe, and `Unknown` is not reported
//! either — but a region that is not there at all is not a region the guard layer can answer `Unknown` about,
//! which is why the gate is here rather than left to it.

use cpp_parser::{CppSyntaxNode, CppTokenKind, SourceRange};

use crate::index::visibility_at;
use crate::preprocess::guard::Visibility;
use crate::FactGuard;

use super::{Checks, Finding};

/// The name this check reports under — see [`Finding::check`].
pub const CHECK: &str = "an_error_the_file_asks_for";

/// Every `#error` the file writes in a branch that is compiled.
pub fn an_error_the_file_asks_for_is_reported(checks: &Checks<'_>) -> Vec<Finding> {
    let directives = directives_in(checks.tree);

    // **The gate** — see the module documentation. A summary with no conditionals for a file that has
    // conditionals is a summary that cannot answer this question, and the answer it must not give is the
    // optimistic one.
    let conditionals_in_the_file = directives
        .iter()
        .filter(|directive| directive.opens_a_conditional)
        .count();
    if conditionals_in_the_file > 0 && checks.summary.guards.conditionals.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for directive in directives {
        let Some(message) = &directive.message else {
            continue;
        };

        // Which conditional region this directive is written in, if any. `None` is file scope, which is
        // compiled — and it is only read that way once the gate above has said the summary describes this
        // file's conditionals at all.
        let guard = match region_containing(&checks.summary.guards, directive.range.start_offset) {
            Some(region) => FactGuard::Region(region),
            None => FactGuard::Unconditional,
        };

        // **The whole point of the check.** `Unknown` — a condition nothing in the index can decide — is not
        // reported: a `#error` that *might* be compiled is not a claim this layer makes.
        if visibility_at(checks.index, checks.path, guard, directive.range.start_offset)
            != Visibility::Active
        {
            continue;
        }

        findings.push(Finding {
            range: directive.range,
            name: String::new(),
            check: CHECK,
            message: if message.is_empty() {
                "this compilation was asked to stop here".to_string()
            } else {
                format!("this compilation was asked to stop here: {message}")
            },
        });
    }

    findings
}

/// One `#error`, as the file's own tree spells it.
struct DiagnosticDirective {
    range: SourceRange,
    /// The message as written, or `None` when this is a `#warning` — which is not this check's to report.
    message: Option<String>,
    /// Whether this is an `#if`-family directive, which is what the gate counts.
    opens_a_conditional: bool,
}

/// **Every directive in the file, read out of the tree rather than out of the text.**
///
/// The tree is the layer that already knows where a directive begins and ends, including the continuation
/// lines that make a `#define` one logical line. A second scanner over the source would be a re-implementation
/// of the lexer, free to disagree with it — the trap `MacroFact::body_range`'s documentation names.
///
/// Only two kinds are kept: the `#error`s (with their messages) and the conditional openers, which exist here
/// solely for the gate. Everything else is dropped as it is seen, so the cost is one walk of the tree.
fn directives_in(root: &CppSyntaxNode) -> Vec<DiagnosticDirective> {
    let mut found = Vec::new();

    for element in root.descendants_with_tokens() {
        let Some(node) = element.into_node() else {
            continue;
        };
        if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::PreprocessorDirective
        {
            continue;
        }

        let tokens: Vec<_> = node
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .filter(|token| !token.text().trim().is_empty())
            .collect();

        // A directive is `#` and a name; anything else is not one this check has an opinion about.
        if tokens.first().map(|token| CppTokenKind::from(token.kind()))
            != Some(CppTokenKind::Hash)
        {
            continue;
        }
        let Some(name) = tokens.get(1) else { continue };

        let range = cpp_parser::source_range(node.text_range());
        let opens_a_conditional = matches!(
            name.text(),
            "if" | "ifdef" | "ifndef"
        );

        let message = (name.text() == "error").then(|| {
            // The message is the rest of the logical line, joined as the file spelled it. Empty is legal and
            // means the directive alone — `#error` with nothing after it still stops the compilation.
            tokens[2..]
                .iter()
                .take_while(|token| !token.text().contains('\n'))
                .map(|token| token.text())
                .collect::<Vec<_>>()
                .join(" ")
        });

        if message.is_none() && !opens_a_conditional {
            continue;
        }

        found.push(DiagnosticDirective {
            range,
            message,
            opens_a_conditional,
        });
    }

    found
}

/// The conditional region whose branch **body** covers `offset`, if one does.
///
/// [`GuardBranch::body`](crate::GuardBranch) is "from after this branch's directive to the next branch's
/// directive, or to the `#endif`", which is exactly the span a directive written inside that branch falls in.
/// Regions are searched **innermost first** — a smaller region index is a parent, per
/// [`ConditionalRegion::parent`](crate::ConditionalRegion) — so a directive nested two deep is attributed to
/// the region that actually contains it rather than to an ancestor that also does.
fn region_containing(guards: &crate::SummaryGuards, offset: usize) -> Option<u32> {
    let mut innermost: Option<(u32, usize)> = None;

    for (index, region) in guards.conditionals.iter().enumerate() {
        for branch in &region.branches {
            let body = branch.body;
            if offset >= body.start_offset && offset < body.end_offset() {
                // Deepest wins: a region nested inside another has the smaller body, and that is the one whose
                // condition decides whether this directive is compiled.
                if innermost.is_none_or(|(_, size)| body.length < size) {
                    innermost = Some((index as u32, body.length));
                }
            }
        }
    }

    innermost.map(|(region, _)| region)
}
