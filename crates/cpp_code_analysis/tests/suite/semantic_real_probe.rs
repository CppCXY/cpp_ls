//! **What the semantic layer says about one real file's macro invocations** — the probe a misclassification is
//! measured with. It reads `CodeFormatCore/src/Config/LuaStyle.cpp` of `EmmyLuaCodeStyle`, where an editor drew
//! `BOOL_OPTION` as a *function declaration* on some lines and as a macro on others.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test -p cpp_code_analysis --test suite semantic_real_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use cpp_code_analysis::semantic::{Name, NameKind, classified_names};
use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

#[test]
#[ignore = "a probe: reads one file of a real project"]
fn every_invocation_of_a_file_s_own_macro_is_a_macro() {
    let Ok(root) = std::env::var("CPPLS_REAL_PROJECT") else {
        eprintln!("set CPPLS_REAL_PROJECT to run this probe");
        return;
    };
    let root = PathBuf::from(root);
    let path = root.join("CodeFormatCore/src/Config/LuaStyle.cpp");
    let Ok(written) = std::fs::read_to_string(&path) else {
        eprintln!("cannot read {}", path.display());
        return;
    };
    let mut files = Vec::new();
    let mut pending = vec![root.join("CodeFormatCore/src")];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry = entry.path();
            if entry.is_dir() {
                pending.push(entry);
            } else if entry.extension().is_some_and(|it| it == "cpp" || it == "h")
                && let Ok(text) = std::fs::read_to_string(&entry)
            {
                files.push((entry.to_string_lossy().to_string(), text));
            }
        }
    }
    let mut memory = MemoryFiles::new();
    for (path, text) in &files {
        memory = memory.with_file(path.clone(), text.clone());
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        &root,
        providers,
        WatchFilter::new(&root),
        CompilerConfig::default(),
    );
    session.add_project_files(files.iter().map(|(path, _)| PathBuf::from(path)));
    session.want_cooked_reading(&path);
    session.index_everything();

    let view = session
        .view_of_the_file(&path)
        .expect("the file has a view");
    let names: Vec<Name> = classified_names(session.index(), &view);
    let mut counts: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for name in &names {
        let start = name.range.start_offset;
        let text = view
            .source
            .get(start..start + name.range.length)
            .unwrap_or("");
        if !text.starts_with("BOOL_OPTION") && !text.starts_with("NUMBER_OPTION") {
            continue;
        }
        let entry = counts.entry(text.to_string()).or_default();
        if name.kind == NameKind::Function {
            entry.0 += 1;
            println!(
                "{text}: FUNCTION at rendered offset {start}, declaration {}",
                name.declaration
            );
        } else {
            entry.1 += 1;
        }
    }
    println!("(function, other) per spelling: {counts:?}");
    let functions: usize = counts.values().map(|(functions, _)| functions).sum();
    assert_eq!(
        functions, 0,
        "every invocation of a macro this file defines is a macro"
    );
    let _ = written;
}

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
