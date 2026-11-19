//! **A `#define` inside an `#if` reaches the rendering** — the environment composed out of summaries evaluates the
//! condition it stored.
//!
//! The report this comes from is `nlohmann/json.hpp`: `JSON_TRY`/`JSON_CATCH` are defined in the two branches of an
//! `#if` in `detail/macro_scope.hpp`,
//!
//! ```cpp
//! #if (defined(__cpp_exceptions) || defined(__EXCEPTIONS) || defined(_CPPUNWIND)) && !defined(JSON_NOEXCEPTION)
//! #define JSON_TRY try
//! #define JSON_CATCH(exception) catch(exception)
//! #else
//! #define JSON_TRY if(true)
//! #define JSON_CATCH(exception) if(false)
//! #endif
//! ```
//!
//! and when **no translation unit covers the header** the environment is composed out of the index's summaries. That
//! composition used to take only unconditional `#define`s — a rule written when there was no condition evaluator at
//! all — so a header whose macros all live behind an `#if` rendered with the invocations still in the text:
//! `JSON_CATCH (std::out_of_range&) { … }` is not a call and not a statement, the parse failed, and every member of
//! `basic_json` written after the first `at` was filed **without its class scope**.
//!
//! The fixture is that shape with no library in it: a macro defined in the `#else` branch, used where only an
//! expansion can produce a declaration.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// A header whose only definition of `GUARDED` is conditional, and a class that can only be read if it expands.
const HEADER: &str = "\
#if defined(EXCEPTIONS_ON) || defined(_CPPUNWIND)
#define GUARDED(x) double x;
#else
#define GUARDED(x) int x;
#endif

struct Widget {
    GUARDED(first)
    GUARDED(second)
    long after;
};
";

const USER: &str = "\
#include \"a.h\"

int use() {
    Widget widget{};
    return widget.after;
}
";

fn the_session() -> Session<MemoryFiles> {
    let memory = MemoryFiles::new()
        .with_file("/p/a.h", HEADER)
        .with_file("/p/b.cpp", USER);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.h"),
        std::path::PathBuf::from("/p/b.cpp"),
    ]);
    session.index_everything();
    session
}

#[test]
fn a_definition_inside_an_if_reaches_the_rendering() {
    let mut session = the_session();
    // **The header on its own** — no translation unit covers it, which is the composed-environment path: the cook
    // has to decide the `#if` from the summary's stored question.
    let _ = session.cook("/p/a.h");
    let facts = session
        .index()
        .cooked_declarations(std::path::Path::new("/p/a.h"))
        .map(|facts| facts.to_vec())
        .unwrap_or_default();
    let names: Vec<String> = facts.iter().map(|fact| fact.qualified_name()).collect();

    assert!(
        names.iter().any(|name| name == "Widget::first"),
        "the `#else` branch defines `GUARDED(x)` as `int x;`, so the class has `first`: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "Widget::second"),
        "and `second`, from the same macro: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "Widget::after"),
        "and the member written after them is still part of the class — this is the symptom of the report, where \
         every member after the first unexpanded macro call lost its scope: {names:?}"
    );
    let first = facts
        .iter()
        .find(|fact| fact.qualified_name() == "Widget::first")
        .expect("the fact");
    assert_eq!(
        first.type_of.as_deref(),
        Some("int"),
        "the type is the one the taken branch wrote"
    );
}

/// **A macro a header defines at its top and takes away at its bottom is defined *in between*.**
///
/// This is `nlohmann`'s exact shape, and it is why the composed environment had to become a **timeline**:
///
/// ```cpp
/// // json.hpp
/// #include "detail/macro_scope.hpp"      // #define JSON_TRY if(true)  … between the two
/// … class basic_json { … JSON_TRY { … } … };
/// #include "detail/macro_unscope.hpp"    // #undef JSON_TRY
/// ```
///
/// A flat table has one answer per name, so the `#undef` at the bottom — being the later one — was the answer for the
/// whole file, the class body rendered with `JSON_CATCH (std::out_of_range&) { … }` in it, and every member after the
/// first `at` was filed without its class scope. Measured on the real header: 6 parse errors and 151 `ClassBody`
/// children before, **0** errors and 242 members of `basic_json` after — `operator[]` among them.
#[test]
fn a_macro_defined_at_the_top_and_undone_at_the_bottom_is_in_force_in_between() {
    const DEFINES: &str = "#define GUARDED(x) int x;\n";
    const UNDEFINES: &str = "#undef GUARDED\n";
    const SCOPED: &str = "\
#ifndef SCOPED_H
#define SCOPED_H
#include \"defines.h\"

struct Scoped {
    GUARDED(first)
    long after;
};

#include \"undefines.h\"
#endif
";
    let memory = MemoryFiles::new()
        .with_file("/p/scoped.h", SCOPED)
        .with_file("/p/defines.h", DEFINES)
        .with_file("/p/undefines.h", UNDEFINES);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/scoped.h"),
        std::path::PathBuf::from("/p/defines.h"),
        std::path::PathBuf::from("/p/undefines.h"),
    ]);
    session.index_everything();
    let _ = session.cook("/p/scoped.h");
    let facts = session
        .index()
        .cooked_declarations(std::path::Path::new("/p/scoped.h"))
        .map(|facts| facts.to_vec())
        .unwrap_or_default();
    let names: Vec<String> = facts.iter().map(|fact| fact.qualified_name()).collect();

    assert!(
        names.iter().any(|name| name == "Scoped::first"),
        "`GUARDED(first)` is in force where the class is written — the `#undef` comes after it, not before: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "Scoped::after"),
        "and the member after it is still part of the class: {names:?}"
    );
}
