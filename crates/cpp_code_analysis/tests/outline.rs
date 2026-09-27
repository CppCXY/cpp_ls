//! What a file's outline is, and the two things it deliberately is **not**.
//!
//! An outline is the one reading in this crate that wants the file's **own** text rather than the cooked one, and
//! both halves of that choice are pinned here: a declaration in a branch nobody takes is in it (a reader editing
//! dead code is looking at it), and a name the file does not write is not. The nesting and the order are pinned
//! too, because they are what a client draws.

use std::path::Path;

use cpp_code_analysis::{DeclKind, OutlineSymbol, SummaryKey, summarize};

/// The outline of a file, as the LSP layer asks for it: the file's own summary, no index, no session.
fn outline_of(source: &str) -> Vec<OutlineSymbol> {
    summarize(Path::new("/p/a.h"), source, SummaryKey::new(0, 0)).outline()
}

/// The names of a list of symbols, for an assertion about structure rather than about facts.
fn names(symbols: &[OutlineSymbol]) -> Vec<&str> {
    symbols.iter().map(|symbol| &*symbol.fact.name).collect()
}

/// **Nesting comes from the facts**, and the order is the file's.
///
/// A member's parent is the fact whose qualified name is the member's scope, so a class's members are its children
/// and a namespace's contents are nested one level deeper. Everything is in source order, which is what a client
/// draws top to bottom.
#[test]
fn namespaces_and_classes_nest_in_source_order() {
    let outline = outline_of(
        "namespace ns {\n\
         struct Widget { int size; void grow(); };\n\
         int free_function();\n\
         }\n\
         int after;\n",
    );

    assert_eq!(names(&outline), vec!["ns", "after"], "two roots, in the file's order");

    let ns = &outline[0];
    assert_eq!(ns.fact.kind, DeclKind::Namespace);
    assert_eq!(
        names(&ns.children),
        vec!["Widget", "free_function"],
        "the namespace's own contents"
    );

    let widget = &ns.children[0];
    assert_eq!(widget.fact.kind, DeclKind::Type);
    assert_eq!(widget.fact.scope.as_deref(), Some("ns"));
    assert_eq!(
        names(&widget.children),
        vec!["size", "grow"],
        "and the class's members are one level deeper"
    );
    assert_eq!(
        widget.children[0].fact.scope.as_deref(),
        Some("ns::Widget"),
        "which is what the scope of a member is"
    );

    // **The whole declaration and the name**, which is the pair a client wants: it folds the first and selects the
    // second, and the second has to be inside the first.
    let outer = widget.fact.range;
    let inner = widget.fact.name_range;
    assert!(
        outer.start_offset <= inner.start_offset && inner.end_offset() <= outer.end_offset(),
        "the name is inside the declaration: {outer:?} vs {inner:?}"
    );
}

/// **A declaration in a branch nobody takes is in the outline.**
///
/// The whole reason this query reads the file's own text: `#if 0` is code the compiler never sees and the reader
/// very much does — an outline that hid it would hide the code the reader is editing. This is also the sense in
/// which the outline is the consumer of the shape rules that read the *raw* tree: whatever those rules can read in
/// dead text shows up here.
#[test]
fn a_branch_nobody_takes_is_still_in_the_outline() {
    let outline = outline_of(
        "#if 0\n\
         struct NeverCompiled { int x; };\n\
         #endif\n\
         struct Always { int y; };\n",
    );

    assert_eq!(
        names(&outline),
        vec!["NeverCompiled", "Always"],
        "both classes, in the order they are written"
    );

    let never = &outline[0];
    assert_eq!(
        names(&never.children),
        vec!["x"],
        "with its members, which are in dead text too"
    );
}

/// **A name the file does not write is not in the outline** — the mirror of the previous test.
///
/// `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND` to a compiler; the file writes a call. The index holds those
/// declarations (that is what the cooked reading is for, and completions offer them), and an outline of this file
/// that listed them would be claiming the file writes a name it never mentions.
#[test]
fn a_name_the_file_does_not_write_is_not_in_the_outline() {
    let outline = outline_of(
        "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
         typedef struct name##__ *name\n\
         DECLARE_HANDLE(HWND);\n",
    );

    assert_eq!(
        names(&outline),
        Vec::<&str>::new(),
        "the macro's call is not a declaration here: {outline:?}"
    );
}

/// **Locals are left out**: an outline is the file's structure, not the inside of every function.
///
/// A summary cannot place a local in the function it belongs to — a local's scope is `None` by construction, and a
/// function body contributes no qualified segment — so the alternative to leaving them out is listing every
/// variable of every body at the top level.
#[test]
fn locals_are_not_in_the_outline() {
    let outline = outline_of(
        "void f() {\n\
         int local;\n\
         struct Inside { int member; };\n\
         }\n\
         int global;\n",
    );

    assert_eq!(
        names(&outline),
        vec!["f", "global"],
        "the function and the file-scope variable, and nothing from inside the body"
    );
    assert!(
        outline[0].children.is_empty(),
        "not even the class declared in the body, which nothing outside it can name: {:?}",
        outline[0].children
    );
}
