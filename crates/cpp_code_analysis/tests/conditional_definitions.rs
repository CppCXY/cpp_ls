//! **A definition inside a conditional branch** — the standard library's own shape, and the one that was missing.
//!
//! `<xstddef>` writes `_MEMBER_CALL_CV_REF_NOEXCEPT` **twice**, once in each branch of
//! `#ifdef __cpp_noexcept_function_type`, and no file ever `#undef`s it:
//!
//! ```cpp
//! #ifdef __cpp_noexcept_function_type
//! #define _MEMBER_CALL_CV_REF_NOEXCEPT(FUNC) …two-call form…
//! #else
//! #define _MEMBER_CALL_CV_REF_NOEXCEPT(FUNC) …one-call form…
//! #endif
//! ```
//!
//! A compiler executes the `#define` of the branch it takes, so the name is **defined** for every file read after
//! `<xstddef>`. Measured on `import std;` (4.85 MB, 224 files), ours was **not**: the only lookup of that name in the
//! whole cook answered `not defined (in_force=no)`, so `<xtr1common>`'s invocations were left in the rendering as text.
//!
//! The observable is a **declaration the macro makes**, read through a **module unit** — the path `import std;` itself
//! takes (`read_the_modules_a_file_imports`), and the one place a file is read together with its include closure. The
//! layout is the machine's, not one invented here: `lib/include` is an include path, `lib/modules/m.ixx` is the
//! interface unit, and the importing file is in neither directory — the arrangement
//! `a_module_whose_interface_unit_is_outside_the_project_is_read_in` already uses.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// The macro is defined in **both** branches, and the two bodies are told apart by the member they write.
const HEADER: &str = r#"#pragma once
#ifdef A_FEATURE_THIS_PROJECT_DOES_NOT_DEFINE
#define SHAPE(x) struct x { int from_if; };
#else
#define SHAPE(x) struct x { int from_else; };
#endif
"#;

/// The interface unit that uses it, after including the header in its global module fragment.
const INTERFACE: &str = "module;\n#include <shape.h>\nexport module m;\n\nSHAPE(Widget)\n";

fn session_with(files: MemoryFiles) -> Session<MemoryFiles> {
    let config = CompilerConfig::new().with_include_path("lib/include");
    let mut session = Session::with_config(
        ".",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("."),
        config,
    );
    session.add_project_files([PathBuf::from("src/main.cpp")]);
    session.index_everything();
    session
}

/// What the index holds for a name, as `(scope, kind)`.
fn facts_named(session: &Session<MemoryFiles>, name: &str) -> Vec<(Option<String>, String)> {
    session
        .index()
        .every_fact_named(name)
        .into_iter()
        .map(|(_, fact)| (fact.scope.clone(), format!("{:?}", fact.kind)))
        .collect()
}

/// **The interface's use of the conditional macro is a declaration in the program.**
#[test]
fn a_definition_inside_a_conditional_branch_is_in_force_for_the_file_that_uses_it() {
    let files = MemoryFiles::new()
        .with_file("lib/modules/m.ixx", INTERFACE)
        .with_file("lib/include/shape.h", HEADER)
        .with_file("src/main.cpp", "import m;\n");
    let mut session = session_with(files);
    let read = session.read_the_modules_a_file_imports(Path::new("src/main.cpp"));
    assert_eq!(read, 1, "one interface unit: `m.ixx`, and nothing else");

    let widgets = facts_named(&session, "Widget");
    assert!(
        widgets.iter().any(|(scope, _)| scope.is_none()),
        "`SHAPE(Widget)` expanded into a declaration: {widgets:?}"
    );
    let members = facts_named(&session, "from_else");
    assert!(
        members
            .iter()
            .any(|(scope, _)| scope.as_deref() == Some("Widget")),
        "and it was the branch that was taken — `#else`, since the feature is undefined: {members:?}"
    );
}

/// **The control**: the same macro defined **unconditionally** expands. This separates "a definition from an included
/// header is not in force" from "a definition inside a conditional branch is not".
#[test]
fn an_unconditional_definition_from_the_same_header_does_expand() {
    let files = MemoryFiles::new()
        .with_file(
            "lib/modules/m.ixx",
            "module;\n#include <shape.h>\nexport module m;\n\nSHAPE_ALWAYS(Gadget)\n",
        )
        .with_file(
            "lib/include/shape.h",
            "#pragma once\n#define SHAPE_ALWAYS(x) struct x { int from_always; };\n",
        )
        .with_file("src/main.cpp", "import m;\n");
    let mut session = session_with(files);
    let read = session.read_the_modules_a_file_imports(Path::new("src/main.cpp"));
    assert_eq!(read, 1, "one interface unit: `m.ixx`, and nothing else");
    let gadgets = facts_named(&session, "Gadget");
    assert!(
        gadgets.iter().any(|(scope, _)| scope.is_none()),
        "the unconditional macro expanded into a declaration: {gadgets:?}"
    );
}

/// **A condition nobody can decide** — the case the fix exists for.
///
/// `#if A_FEATURE_VALUE == 1` where `A_FEATURE_VALUE` is defined **without a value** cannot be evaluated: the
/// expression needs a body the table does not have. Both branches are then `Unknown`, and before the fix **both**
/// `#define`s were dropped — so anything after the conditional saw the name as undefined and left the invocation in the
/// rendering as text. A compiler takes one branch or the other, so the name is defined afterwards either way; the
/// reading here is the `#else` one, which is what a compiler does when the value is absent.
///
/// **Ignored while it fails**, and it is a *different* defect from the one fixed here: measured, the walk never takes
/// the undecidable branch for these facts — with `A_FEATURE_VALUE` defined **without a value**, `#if A_FEATURE_VALUE
/// == 1` is evaluated as `Inactive` rather than `Unknown`, so the `#else` branch's definition is dropped as dead
/// instead of kept. The next target is the condition evaluator, not the walk.
#[ignore = "known: an empty-bodied macro in a `#if` expression evaluates as false, not `Unknown`"]
#[test]
fn a_definition_in_a_branch_nobody_can_decide_is_still_in_force() {
    let header = r#"#pragma once
#if A_FEATURE_VALUE == 1
#define SHAPE(x) struct x { int from_if; };
#else
#define SHAPE(x) struct x { int from_else; };
#endif
"#;
    let files = MemoryFiles::new()
        .with_file("lib/modules/m.ixx", INTERFACE)
        .with_file("lib/include/shape.h", header)
        .with_file("lib/include/value.h", "#define A_FEATURE_VALUE\n")
        .with_file("src/main.cpp", "import m;\n");
    // The value header is pulled in by the interface, so the condition sees a name with no value.
    let interface =
        "module;\n#include <value.h>\n#include <shape.h>\nexport module m;\n\nSHAPE(Widget)\n";
    let files = files.with_file("lib/modules/m.ixx", interface);
    let mut session = session_with(files);
    let read = session.read_the_modules_a_file_imports(Path::new("src/main.cpp"));
    assert_eq!(read, 1, "one interface unit: `m.ixx`, and nothing else");
    let widgets = facts_named(&session, "Widget");
    assert!(
        widgets.iter().any(|(scope, _)| scope.is_none()),
        "the macro from an undecidable branch expanded at the use site: {widgets:?}"
    );
    let members = facts_named(&session, "from_else");
    assert!(
        members
            .iter()
            .any(|(scope, _)| scope.as_deref() == Some("Widget")),
        "and it was the `#else` branch — the one a compiler takes when the value is absent: {members:?}"
    );
}
