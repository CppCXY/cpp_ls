//! **`missing-include`**: a type this file uses, declared in a header it does not include (or a module it does not
//! import).
//!
//! ```cpp
//! // widget.cpp
//! void Widget::grow(int by) { … }     // `Widget` comes from `widget.h`, which this file never includes
//! ```
//!
//! # Why this is a different rule from `undefined-name`
//!
//! `undefined-name` asks "does anything declare this, from here" and reports when the answer is no. This asks the
//! **next** question: *something* declares it — in a header the file never included — so the program compiles only
//! because another translation unit's include order happened to pull that header in first. That state is fragile
//! (reordering two `#include`s breaks the build) and it is exactly what the fix repairs, which is why the finding and
//! the `source.include` code action are two halves of one feature.
//!
//! # What it reports, and what it refuses to
//!
//! Only **types a declaration spells**: the declared type of a variable, a field or a return value, and the types in a
//! parameter list. That is where a missing include bites, and it is a position the file's own facts already record
//! (`DeclFact::type_of`, `DeclFact::parameter_list`) — so the rule costs no tree walk.
//!
//! ```text
//!   reported      Widget w;            a type in a declaration
//!                 void f(Widget* p);   a type in a parameter list
//!   not reported  w.grow()             a member access — `undefined-name`'s question
//!                 ns::Widget w;        a qualified spelling: a namespace question, not this one
//!                 T x;                 a template parameter: no file declares it, so no candidate
//! ```
//!
//! # How it decides "not visible", and why that is *not* the visibility walk
//!
//! The honest question is [`ProjectIndex::definition`]'s — but that is the full visibility walk, which evaluates every
//! condition on the way: **1.2 s** on a file whose closure is 600 files, per keystroke, for a rule that only needs to
//! know whether a *header* is already in the file's view. So the rule asks the graph instead
//! ([`ProjectIndex::include_closure`]: the file's transitive `#include` closure, no guard evaluation) and treats a
//! candidate header in that set as visible. That is conservative in the only direction that matters: it can miss a
//! name behind an inactive guard, and it never invents one.
//!
//! # The standard library is excluded mechanically, not by name
//!
//! Suggesting `<string>` would need the standard library's whole index, which costs more than the finding is worth — so
//! a candidate is only considered when its file is **ours**: under the workspace root, or under one of the directories
//! the project's own files are in (`CheckContext::inside`). Every toolchain header, every module unit the session read
//! for `import std;`, and every vendored tree outside the workspace is therefore invisible to this rule without a
//! single name being listed.
//!
//! # Why it can be turned off
//!
//! `[diagnostics] disable = ["missing-include"]` — the mechanism every rule already uses. A rule about *the style of
//! inclusion* is the kind a project wants uniformly or not at all, and a reader taught to ignore one squiggle has
//! turned the channel off; the switch makes that a decision rather than a habit.

use std::path::Path;

use super::{Check, CheckContext, DiagnosticCode};
use crate::DeclKind;

/// **`missing-include`** — see the module documentation.
pub struct MissingInclude;

impl Check for MissingInclude {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::MissingInclude];

    fn check(context: &mut CheckContext<'_>) {
        let path = context.path.to_path_buf();
        // **The names this file declares itself** — a type written in a declaration of its own is not a missing
        // include, and a class's own members are its own business.
        let own: std::collections::HashSet<String> = context
            .summary
            .declarations
            .iter()
            .map(|fact| fact.name.clone())
            .collect();

        // **The files this file already reaches**, by `#include` — the cheap half of visibility, and the one that
        // decides whether a candidate header is worth mentioning at all. A graph walk with no guard evaluation: see
        // `ProjectIndex::include_closure` for why the full walk is not affordable on a keystroke.
        let reached = context.index().include_closure(&path);

        let mut reported: std::collections::HashSet<String> = std::collections::HashSet::new();
        for fact in &context.summary.declarations {
            // Only a declaration: a body's locals are the scope tree's question, and `auto` is not a type at all.
            if fact.local || matches!(fact.kind, DeclKind::MacroLike | DeclKind::Other) {
                continue;
            }
            for spelling in types_this_declaration_uses(fact) {
                // **A qualified spelling is not this rule's** — `asio::io_context` would name `asio` (a namespace),
                // and the fix cannot express "include the header for a nested name" without guessing. Measured on a
                // real project: without this, `function`, `less` and `asio` were reported from qualified spellings.
                if is_a_qualified_spelling(&spelling) {
                    continue;
                }
                for name in type_names_in(&spelling) {
                    if name == fact.name
                        || own.contains(&name)
                        || is_a_qualified_spelling(&name)
                        || !reported.insert(name.clone())
                    {
                        continue;
                    }
                    let Some(header) = the_candidate(context, &name, &path, &reached) else {
                        continue;
                    };
                    let message = format!(
                        "`{name}` is declared in {header}, which this file does not include"
                    );
                    context.add_in_the_file(
                        DiagnosticCode::MissingInclude,
                        fact.range,
                        name,
                        message,
                    );
                }
            }
        }
    }
}

/// **The strongest file that declares `name`, is ours, and is not already reached** — as a name a message can print.
///
/// Ranking is what keeps the finding readable: a header beside the file, or one named after the type, is what a reader
/// means; the fifth `Util` in another target's directory is not. Only the first is reported, and the code action
/// recomputes the list rather than being told which one it was.
fn the_candidate(
    context: &CheckContext<'_>,
    name: &str,
    path: &Path,
    reached: &std::collections::HashSet<String>,
) -> Option<String> {
    let mut best: Option<(u8, String)> = None;
    for (declaring, fact) in context.index().every_fact_named(name) {
        if declaring == path || !is_ours(context, &declaring) {
            continue;
        }
        // **A type is what a declaration spells**; a same-named function or variable elsewhere is a different answer
        // to a different question.
        if fact.kind != DeclKind::Type {
            continue;
        }
        // **Already reached** by an `#include`: visible as far as this rule is concerned, and a header the file
        // includes is never suggested back to it.
        if reached.contains(&crate::file::paths::normalize_path(
            &declaring,
            cfg!(windows),
        )) {
            continue;
        }
        let rank = how_strongly_it_answers(&declaring, name, path);
        if best.as_ref().is_none_or(|(known, _)| rank < *known) {
            let file = declaring
                .file_name()
                .map(|file| file.to_string_lossy().to_string())
                .unwrap_or_else(|| declaring.display().to_string());
            best = Some((rank, file));
        }
    }
    best.map(|(_, file)| file)
}

/// **How well a file answers "where is this type from"** — lower is stronger.
///
/// The numbers are a ranking, not a claim about the language: a header in the same directory is what a reader almost
/// always means, one named after the type is next, and anything else is a candidate the message names without
/// pretending to be sure.
fn how_strongly_it_answers(declaring: &Path, name: &str, asking: &Path) -> u8 {
    let same_directory = declaring.parent() == asking.parent();
    let named_after_it = declaring.file_stem().is_some_and(|stem| {
        let stem = stem.to_string_lossy().to_lowercase();
        let name = name.to_lowercase();
        stem == name || stem.strip_suffix('s') == Some(name.as_str())
    });
    match (same_directory, named_after_it) {
        (true, true) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (false, false) => 3,
    }
}

/// Is this file **ours** — under the workspace root, or under a directory the project's own files are in?
///
/// This is the whole of the "never suggest the standard library" rule, and it is mechanical on purpose: a toolchain
/// header, a module unit read for `import std;`, or a vendored tree outside the project is not a file this project may
/// add an `#include` for.
fn is_ours(context: &CheckContext<'_>, file: &Path) -> bool {
    let file = file.to_string_lossy().to_lowercase().replace('\\', "/");
    context.inside.iter().any(|root| {
        let root = root.to_string_lossy().to_lowercase().replace('\\', "/");
        let root = root.trim_end_matches('/');
        !root.is_empty() && file.starts_with(root)
    })
}

/// The type spellings a declaration uses — its own type, and the types in its parameter list.
fn types_this_declaration_uses(fact: &crate::DeclFact) -> Vec<String> {
    let mut spellings: Vec<String> = Vec::new();
    if let Some(of) = &fact.type_of {
        spellings.push(of.clone());
    }
    if let Some(parameters) = &fact.parameter_list {
        spellings.extend(parameters.split(',').map(|one| one.trim().to_string()));
    }
    spellings
}

/// **The names a type spelling mentions** — `const Widget&` gives `Widget`.
///
/// Deliberately crude: the words a spelling is made of, minus the language's own, are exactly the names a missing
/// include can be about. A spelling with a `::` in it is skipped by the caller, because the fix cannot express "include
/// the header for a template argument" without guessing.
fn type_names_in(spelling: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut word = String::new();
    for character in spelling.chars().chain(std::iter::once(' ')) {
        if character.is_alphanumeric() || character == '_' {
            word.push(character);
            continue;
        }
        if !word.is_empty() {
            if !is_a_keyword(&word) {
                names.push(word.clone());
            }
            word.clear();
        }
    }
    names
}

/// The language's own words, which are not types a header declares.
fn is_a_keyword(word: &str) -> bool {
    matches!(
        word,
        "const"
            | "volatile"
            | "static"
            | "inline"
            | "mutable"
            | "explicit"
            | "virtual"
            | "constexpr"
            | "consteval"
            | "constinit"
            | "extern"
            | "register"
            | "thread_local"
            | "signed"
            | "unsigned"
            | "short"
            | "long"
            | "int"
            | "char"
            | "bool"
            | "float"
            | "double"
            | "void"
            | "auto"
            | "wchar_t"
            | "char8_t"
            | "char16_t"
            | "char32_t"
            | "struct"
            | "class"
            | "union"
            | "enum"
            | "typename"
            | "template"
            | "noexcept"
            | "override"
            | "final"
            | "and"
            | "or"
            | "not"
    )
}

/// Was this written with a `::`? Those are a namespace question, and the fix cannot spell them without guessing.
fn is_a_qualified_spelling(spelling: &str) -> bool {
    spelling.contains("::")
}
