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
//!                 Container _hash;     a name this file's own scopes bind — a template parameter
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
//! # A header is a fragment of its includers' order, not a translation unit
//!
//! A header is not compiled on its own: its text is spliced into whichever translation unit includes it, and that file
//! owns the include order. `EmmyLuaCodeStyle`'s `LuaStyleStruct.h` uses `FunctionSingleArgSpace` from `LuaStyleEnum.h`
//! and includes neither, and the project compiles because `LuaStyle.h` includes the enum **first** and the struct
//! second; `CodeFormatLib/src/Types.h` and `DiagnosticType.h` have the same shape through the diagnostic headers of
//! `LuaCodeFormat.h`. Both were reported — "declared in `luastyleenum.h`, which this file does not include" — which is a
//! statement about a style the project has not adopted, not about a program that does not compile.
//!
//! So for a file a compiler does not compile on its own, the finding is made about **the files that include it**: the
//! name is missing only if some includer does not already reach the declaring header. When every includer does, the
//! program compiles as written and the rule says nothing. A header nothing includes is reported, because nothing makes
//! the name visible in it at all.
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

        // **And every name this file's own scopes *bind*** — which is a wider set than `summary.declarations`, and
        // the difference is a template parameter. `template <class Container> class DBase { Container _hash; };`
        // declares `Container` in the `template <…>` header, and a template parameter is a **binding** rather than a
        // declaration fact: so the loop below treated `Container` as a name from elsewhere, found a `class Container`
        // in another file of the project, and told the reader to include it — reported from an editor, about a
        // parameter of the class on screen.
        //
        // The scope tree answers it directly and costs one pass over the file's bindings, which is nothing beside
        // the candidate search it saves. A name bound here — a parameter, a local, an enumerator — is not a name this
        // file forgot to include; the rule's whole premise is "nothing declares it **here**".
        let bound: std::collections::HashSet<&str> = context
            .view
            .scopes
            .scopes()
            .iter()
            .flat_map(|scope| scope.bindings.iter())
            .filter_map(|binding| binding.name.identifier_text())
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
                        || bound.contains(name.as_str())
                        || is_a_qualified_spelling(&name)
                        // **A template-id inside a template is the shape a dependent name has.** In
                        // `template <typename T> void get(typename constraint<is_convertible<T&, X&>::value>::type = 0)`
                        // the `is_convertible` may be a member of a dependent base, an ADL-only name, or a name the
                        // instantiation supplies — this rule works from declaration *spellings* and has no scopes for
                        // the use, so the only honest answer is silence. Measured on `EmmyLuaCodeStyle`: the remaining
                        // finding after the first sweep was exactly this (`is_convertible` "declared" in an unrelated
                        // header). A plain type in a template's parameter list — `template <class T> void f(Widget w)`
                        // — is not this shape and is still reported.
                        || (!fact.parameters.is_empty() && is_used_with_template_arguments(&spelling, &name))
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
        // **A source file is never the header to include.** A declaration in a `.cc`, a `.c` or a `.cpp` is not made
        // visible by including it — a source file is a translation unit, and `#include "x.cc"` is not the fix this
        // rule's message proposes. Measured on `EmmyLuaCodeStyle`: four asio headers were told to include
        // `gtest_unittest.cc`, because a template parameter named `T` is *also* a type fact in a test file.
        if is_a_source_file(&declaring) {
            continue;
        }
        // **A declaration this file already reaches settles the name, and that is a different question from "is the
        // *definition* in an included header".**
        //
        // ```cpp
        // #include "mark.h"        // `class LuaParser;`
        // void BuildTree(LuaParser &p);
        // ```
        //
        // The reference needs a declaration, not a definition, and the forward declaration is in a header this file
        // includes — but the rule looked only for facts whose *file* it had not reached, found `luaparser.h` (the
        // class's definition) and told the reader to include it. Reported from `EmmyLuaCodeStyle`'s `LuaSyntaxTree.h`,
        // which compiles as written. So a reached declaration of the name means nothing is missing, whatever its kind
        // and wherever the definition lives.
        if reached.contains(&crate::file::paths::normalize_path(
            &declaring,
            cfg!(windows),
        )) {
            return None;
        }
        // **A header is a fragment of its includers' order**, and this is the file the missing include would break —
        // see `an_includer_misses` and the module documentation. A translation unit answers for itself, which is the
        // case the rule was written for.
        if !is_a_source_file(path) && !an_includer_misses(context, path, &declaring) {
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

/// **Does some file that includes `path` fail to reach `declaring`?** — the question a header's finding is really about.
///
/// A header is text spliced into a translation unit, and the unit's include order is what decides whether the name is
/// visible. `LuaStyleStruct.h` uses `FunctionSingleArgSpace` and includes neither it nor its header, and the program
/// compiles because `LuaStyle.h` includes `LuaStyleEnum.h` first — measured on `EmmyLuaCodeStyle`, where this and
/// `CodeFormatLib/src/Types.h` (`DiagnosticType`) were the rule's only two findings and both were this shape.
///
/// So the finding belongs to the file whose translation unit it breaks: one includer that does not reach the declaring
/// header. `true` when there is no includer at all, because then nothing makes the name visible in this file — no
/// include order exists to fix it.
fn an_includer_misses(context: &CheckContext<'_>, path: &Path, declaring: &Path) -> bool {
    let declaring = crate::file::paths::normalize_path(declaring, cfg!(windows));
    let includers = context.index().includers_of(path);
    if includers.is_empty() {
        return true;
    }
    includers.iter().any(|includer| {
        !context
            .index()
            .include_closure(includer)
            .contains(&declaring)
    })
}

/// **Is this a file a translation unit is compiled from, rather than a header to include?**
///
/// The extension is the whole question: `.c`, `.cc`, `.cpp`, `.cxx` and their friends are *compiled*, and a
/// declaration inside one is not made visible by naming it in an `#include`. A header — `.h`, `.hpp`, `.hxx`, `.inl`,
/// or a file with no extension at all, which is how the standard library writes several of them — is.
fn is_a_source_file(file: &Path) -> bool {
    matches!(
        file.extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .as_deref(),
        Some("c" | "cc" | "cpp" | "cxx" | "c++" | "cp" | "cu" | "m" | "mm" | "ixx" | "cppm")
    )
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

/// **Is `name` written with template arguments here?** — `is_convertible<A, B>`, and not `Widget`.
///
/// The question is only asked of a *template*'s declaration, where a name used this way may be dependent; see the
/// skip that calls it. Whitespace between the name and its `<` is allowed, because a replacement-list macro often
/// puts it there (`completion_handler_for <T>`).
fn is_used_with_template_arguments(spelling: &str, name: &str) -> bool {
    spelling
        .match_indices(name)
        .any(|(at, _)| spelling[at + name.len()..].trim_start().starts_with('<'))
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
