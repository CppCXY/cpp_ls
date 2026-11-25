//! **One member name's whole overload set in one class** — the plural of [`direct_member`].
//!
//! `direct_member` answers *one* declaration on purpose: a jump from a name needs one place to go, and the doc
//! beside the rule says so ("a jump takes the first, a list takes them all"). A **call** is the other question — the
//! arguments choose — and the piece it needs was missing: `member_fact` answers `Known::Yes(one fact)` and
//! [`members_of`] answers every *member* of the class, which walks the bases and reads all of them for a question
//! about one name.
//!
//! Measured on a two-overload fixture, `struct S { int get(int); double get(const char*); };`:
//!
//! ```text
//! s.get(0)    int      ✓  the first declaration happens to be the one `0` selects
//! s.get("z")  int      ✗  a compiler answers `double`
//! s[0]        int&     ✓
//! s["z"]      int&     ✗  a compiler answers `double&`
//! ```
//!
//! Both wrong answers are the *first* declaration, and the ranking that would fix them already exists
//! (`crate::sema::overload::choose`, used by `the_declaration_the_arguments_pick` for free functions). What it needs
//! is the candidate list, which is what this is: the declarations in the class's scope whose name matches, in the
//! order the index files them, with the same alias and partial-specialization rules `direct_member` uses.
//!
//! It is a **scope** query rather than a member-list walk, which is what makes it affordable on a diagnostics pass: a
//! member list substitutes every member of a class and walks its bases (measured: 2.0–2.5 ms warm for
//! `std::vector<int>` and `std::basic_string<char>`, 30 ms cold), while `declarations_in` is one posting query.
//! Overloads of one name live in one class scope by the language's own rule — a base's overload set is *hidden* by a
//! derived declaration — so the plural query is the same cost as the singular one.

use super::*;

/// **Every declaration of `member` in `class`**, in declaration order — the set a call's arguments choose from.
///
/// The returned path is where the declaration's body is: the file the fact was read from, which is what a caller
/// reading a member's body for a deduced return needs.
pub(crate) fn member_candidates(
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
    arguments: Option<&[String]>,
) -> Vec<(DeclFact, PathBuf)> {
    // The same first step as `direct_member`: an alias has no members of its own.
    let class = &resolve_aliases(shapes, index, scopes, root, path, class);

    // **A class in the file being edited is read from its tree**, which is what makes the query work on a buffer that
    // has never been written to disk. Bindings are in declaration order, so the facts come out in it.
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(scope_data) = scopes.scope(scope)
    {
        let declared: Vec<(DeclFact, PathBuf)> = scope_data
            .bindings
            .iter()
            .filter(|binding| binding.name.identifier_text() == Some(member))
            .map(|binding| {
                (
                    fact_from_binding_with(
                        crate::sema::declarations::shapes_from(shapes, root),
                        root,
                        class,
                        binding,
                    ),
                    path.to_path_buf(),
                )
            })
            .collect();
        if !declared.is_empty() {
            return declared;
        }
    }

    // **The class's arguments come off its spelling**, so the partial-specialization rule can compare them; see
    // `direct_member`, which documents why the pattern test is what picks a specialization rather than its primary.
    let from_the_spelling: Option<Vec<String>> = class.split_once('<').map(|(_, rest)| {
        crate::sema::builtins::split_arguments(rest.trim_end().strip_suffix('>').unwrap_or(rest))
    });
    let (bare, written) = match (class.split_once('<'), from_the_spelling.as_ref()) {
        (Some((head, _)), Some(written)) => (head.trim(), Some(written.as_slice())),
        _ => (class.as_str(), arguments),
    };
    let all = index.declarations_in(bare, path);
    // **What the query found, before the pattern filter.** A candidate list of zero is not distinguishable from "the
    // class has no such member" without this line, and it is the line that found the alias defect this query had:
    // `basic_json::operator[]` answered 0 while `members_of("basic_json")` listed 8 of them, because the alias step
    // handed it the unqualified spelling (see `operator_returns_of_with_arguments`).
    let _named_of = |declarations: &[crate::VisibleDeclaration<'_>]| -> usize {
        declarations
            .iter()
            .filter(|declaration| declaration.fact.name == member)
            .count()
    };

    let from_the_index: Vec<(DeclFact, PathBuf)> = all
        .into_iter()
        .filter(|declaration| {
            declaration.fact.name == member
                && a_pattern_matches(
                    declaration.fact.pattern(),
                    declaration.fact.parameters(),
                    written,
                )
        })
        .map(|declaration| (declaration.fact.clone(), declaration.file.clone()))
        .collect();
    if !from_the_index.is_empty() {
        return from_the_index;
    }

    // Nothing is scoped to that class: a member defined out of line can be filed under `C::f`, which is what this
    // last lookup is for.
    match index.definition(&format!("{class}::{member}"), path) {
        Known::Yes(found) => vec![(found.fact, found.file)],
        Known::Unknown(_) | Known::No => Vec::new(),
    }
}
