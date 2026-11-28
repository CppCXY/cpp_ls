//! **What the semantic layer says about one real file's macro invocations** — the probe a misclassification is
//! measured with. It reads `CodeFormatCore/src/Config/LuaStyle.cpp` of `EmmyLuaCodeStyle`, where an editor drew
//! `BOOL_OPTION` as a *function declaration* on some lines and as a macro on others.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test -p cpp_code_analysis --test suite semantic_real_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use cpp_code_analysis::semantic::{NameKind, classified_names};
use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};
/// **The smallest shape the parser misreads**: two invocations of the same function-like macro on one line, which the
/// declaration rule reads as a type (the first) and a declarator (the second) — so the second is a *function* unless
/// the semantic layer knows the name is a macro.
#[test]
fn two_invocations_on_one_line_are_both_macros() {
    let source = "#define BOOL_OPTION(op) { op = true; }\nvoid f() {\n    bool a = false;\n    bool b = false;\n    BOOL_OPTION(a) BOOL_OPTION(b)\n}\n";
    let files = vec![("/p/lua_style.cpp".to_string(), source.to_string())];
    let mut memory = MemoryFiles::new();
    for (path, text) in &files {
        memory = memory.with_file(path.clone(), text.clone());
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([PathBuf::from("/p/lua_style.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/lua_style.cpp"));
    session.index_everything();
    let view = session
        .view_of_the_file(std::path::Path::new("/p/lua_style.cpp"))
        .expect("a view");
    let names = classified_names(session.index(), &view);
    let mut kinds = Vec::new();
    for name in &names {
        let start = name.range.start_offset;
        if view
            .source
            .get(start..start + name.range.length)
            .unwrap_or("")
            == "BOOL_OPTION"
        {
            kinds.push((name.kind, name.declaration));
        }
    }
    println!("one-line pair: {kinds:?}");
    assert!(
        kinds.iter().all(|(kind, _)| *kind == NameKind::Macro),
        "both invocations are the macro: {kinds:?}"
    );
}
