//! **The reading rules this layer follows, checked over its own source.**
//!
//! `sema` is where the crate once answered questions about the tree with **words**: a declaration's text sliced between two
//! offsets and searched for `&`, a member's type read as the source before its name, a local's `static` found by splitting the
//! declaration into whitespace, a node kind compared against another kind the typed layer already names. The sweep that replaced
//! those is only durable if a reintroduction fails a test, so this file reads `src/sema/**/*.rs` and refuses the shapes that are
//! gone.
//!
//! # What is refused, and why each has no exception
//!
//! ```text
//!   CppSyntaxKind::from(…) == CppSyntaxKind::X   the typed layer answers every one of these: `CppX::can_cast(…::kind().into())`.
//                                                A *match* over kinds is not refused — the scope walker's dispatch is one, and
//                                                the README calls it the engine rather than a question about a node.
//   .rfind(&…) / .rfind("…")                     a string searched for inside another string. Splitting a **spelling** is the
//                                                one thing this forbids a list of (below); locating a node's own text is not.
// ```
//!
//! # What is allowed, with the list
//!
//! `.rfind("…")` and `split_whitespace()` remain where the input **is** a spelling rather than a tree: a type written in a
//! template parameter, a qualified callee's `::`, a constraint's trailing `>`. The list is a list so that a new site has to be
//! justified by adding a file, not by writing the call.

use std::path::{Path, PathBuf};

/// **The files where a string is legitimately searched**, each because its input is a spelling and not a node.
/// **The kinds no typed node names**, so the equality is the only way to ask. Each is listed here rather than escaped in place, so
/// that adding one is a review decision: a module header's name, a macro invocation, the translation unit itself (the root, which
/// has no type of its own), a `BuiltinType` (which `CppEnumDef` also accepts, so its `can_cast` is wider than the question), and a
/// template argument list.
const NO_TYPED_NODE: &[&str] = &[
    "ModuleName",
    "MacroCall",
    "TranslationUnit",
    "BuiltinType",
    "TemplateArgumentList",
    // A template header, which declares nothing on its own — the declaration it heads is its sibling.
    "TemplateDecl",
];

const SPELLING_FILES: &[&str] = &[
    // `deduction.rs` reads the `template` in a type's *written* form and splits a parameter spelling into words.
    "deduction.rs",
    // `resolve.rs` searches a completion line for the needle a cursor is on; the needle is not a node.
    "resolve.rs",
    // `types.rs` finds where a name is written inside the parameter's spelling, to take the constraint before it.
    "types.rs",
    // `param_type_check.rs` splits a qualified callee by its last `::`.
    "param_type_check.rs",
];

fn sema_files() -> Vec<(PathBuf, String)> {
    fn walk(directory: &Path, found: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                found.push((path, text));
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("sema");
    let mut found = Vec::new();
    walk(&root, &mut found);
    found
}

fn lines_with(text: &str, needle: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .map(|(index, line)| (index + 1, line.trim().to_string()))
        .collect()
}

fn offenders(needle: &str) -> Vec<String> {
    let mut offenders = Vec::new();
    for (path, text) in sema_files() {
        for (line, code) in lines_with(&text, needle) {
            offenders.push(format!("{}:{line}: {code}", path.display()));
        }
    }
    offenders
}

/// **A kind equality is a question the typed layer already answers.**
///
/// `CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ClassBody` is `CppClassBody::can_cast(node.kind().into())` — and the
/// equality form is what went quiet twice in this crate's history: a definition is not a `FunctionDef`, and `struct` is a
/// `StructDef`.
#[test]
fn no_kind_equality_replaces_a_typed_question() {
    let mut offenders = offenders("CppSyntaxKind::from(");
    offenders.retain(|offender| {
        if !(offender.contains("== CppSyntaxKind::") || offender.contains("!= CppSyntaxKind::")) {
            return false;
        }
        !NO_TYPED_NODE
            .iter()
            .any(|kind| offender.contains(&format!("CppSyntaxKind::{kind}")))
    });
    assert!(
        offenders.is_empty(),
        "a kind equality is a typed question's stand-in — ask `CppX::can_cast(kind.into())`:\n{}",
        offenders.join("\n")
    );
}

/// **A string is not searched for another string's position**, except where the input is a spelling.
#[test]
fn text_is_not_searched_for_structure() {
    let mut offenders = Vec::new();
    for (path, text) in sema_files() {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if SPELLING_FILES.contains(&name.as_str()) {
            continue;
        }
        for needle in [".rfind(\"", ".rfind(&"] {
            for (line, code) in lines_with(&text, needle) {
                offenders.push(format!("{}:{line}: {code}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a node's own text is being located by searching a string — use its range or its typed accessor:\n{}",
        offenders.join("\n")
    );
}

/// **Whitespace splitting stays where a spelling is normalised.**
///
/// `split_whitespace` over a **declaration's** text is how a local's `static` and a typedef's `typedef` were once found — and it
/// finds the words of a comment and a string literal too. The remaining sites normalise a spelling (`const std::string &` →
/// `const std::string &` shape questions), and the list keeps a new one from appearing without a reason.
#[test]
fn whitespace_splitting_is_a_spelling_rule() {
    let allowed = [
        "constraint.rs",
        "declarations.rs",
        "deduction.rs",
        "model.rs",
        "overload.rs",
        "types.rs",
        "concept_not_satisfied.rs",
        "macro_parentheses.rs",
        "mod.rs",
    ];
    let mut offenders = Vec::new();
    for (path, text) in sema_files() {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if allowed.contains(&name.as_str()) {
            continue;
        }
        for (line, code) in lines_with(&text, "split_whitespace()") {
            offenders.push(format!("{}:{line}: {code}", path.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "`split_whitespace` over a node's text is a word search, not a reading — use the typed accessor:\n{}",
        offenders.join("\n")
    );
}
