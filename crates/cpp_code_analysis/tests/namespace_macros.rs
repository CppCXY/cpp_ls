//! **The macro layer, with the standard library's own shape** — the place the user's report points at: "STL defines its
//! namespaces through macros, a begin and an end one".
//!
//! MSVC's `<yvals_core.h>` writes:
//!
//! ```cpp
//! #define _STD_BEGIN namespace std {
//! #define _STD_END   }
//! ```
//!
//! A pair whose **second body is a single `}`**. If the directive scanner records that body as empty — or not at all —
//! the cook expands every `_STD_BEGIN` into `namespace std {` and every `_STD_END` into nothing, and the namespaces
//! accumulate across the stitched unit: measured on `C:\Users\zc\Desktop\cpp_modules`, `_mm_abs_epi16` sits inside
//! **ten** nested `std` namespaces, `vector<bool>` inside eight, and every one of the 6 732 intrinsics is filed under
//! `std`. The tests here are the floor under that: a macro body is whatever follows the name, `}` included.

use std::path::PathBuf;

use cpp_code_analysis::{
    DiskFiles, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// The definitions, in the shape `<yvals_core.h>` writes them.
const DEFINITIONS: &str = "\
#define _STD_BEGIN namespace std {
#define _STD_END   }
";

fn session_with(definitions: &str, body: &str) -> (Session<MemoryFiles>, String) {
    let text = format!("{definitions}{body}");
    let files = MemoryFiles::new().with_file("/p/std_like.h", text.as_str());
    let mut session = Session::with_config(
        "/p",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("/p"),
        cpp_code_analysis::include::config::CompilerConfig::default(),
    );
    session.add_project_files([PathBuf::from("/p/std_like.h")]);
    session.index_everything();
    (session, text)
}

/// **The macro a begin/end pair is** — both bodies, exactly as written.
#[test]
fn a_namespace_pair_macro_keeps_both_bodies() {
    let (session, text) = session_with(DEFINITIONS, "");
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/std_like.h"))
        .expect("the file is indexed");
    // **The body text**, from the range the fact records: a macro whose body is a single `}` has no `value`, and its
    // range is the only place the `}` is written down.
    let body_of = |name: &str| {
        summary
            .macros
            .iter()
            .find(|macro_| macro_.name == name)
            .and_then(|macro_| macro_.body_range)
            .map(|range| {
                text[range.start_offset..range.end_offset()]
                    .trim()
                    .to_string()
            })
    };
    assert_eq!(
        body_of("_STD_BEGIN").as_deref().map(str::trim),
        Some("namespace std {"),
        "the opening macro's body is the namespace it writes"
    );
    assert_eq!(
        body_of("_STD_END").as_deref().map(str::trim),
        Some("}"),
        "and the closing macro's body is a `}}` — the one token a body can be that is easy to lose"
    );
}

/// **What the cook makes of a use** — the two expansions together, nothing dropped.
///
/// The cook is asked directly rather than through a session: a file with no `#include` has nothing to splice, so a
/// session keeps its own text and never renders it.
#[test]
fn cooking_a_namespace_pair_leaves_balanced_braces() {
    let source = format!("{DEFINITIONS}_STD_BEGIN\nint a;\n_STD_END\n");
    let (tokens, _) = cpp_parser::lex(&source, &cpp_parser::LexerConfig::default());
    let rendered = cpp_code_analysis::preprocess::cooked::cook_with(
        &source,
        &tokens,
        &cpp_code_analysis::macros::MacroTable::new(),
    )
    .render()
    .text;
    println!("rendered: {rendered:?}");
    let opens = rendered.matches('{').count();
    let closes = rendered.matches('}').count();
    assert_eq!(
        (opens, closes),
        (opens, opens),
        "the rendering is brace-balanced: {rendered:?}"
    );
    assert!(
        !rendered.contains("_STD_BEGIN") && !rendered.contains("_STD_END"),
        "both macros were expanded: {rendered:?}"
    );
    assert!(
        rendered.contains("namespace std"),
        "the opening macro wrote its namespace: {rendered:?}"
    );
}

/// The fixture file is not needed by the tests above, but a session over a `DiskFiles` provider must not be required
/// to compile them — this keeps the import honest.
#[allow(dead_code)]
fn unused(_: DiskFiles) {}
