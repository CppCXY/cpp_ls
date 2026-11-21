//! **A macro an included header `#undef`s is not a macro at the use** — the cook must see the `#undef`.
//!
//! Reported from a production project: a file that includes a precompiled header, which includes the platform's
//! `min`/`max` macros and then `#undef`s them (the usual Windows shape, because `std::min` cannot be written while the
//! macro is in force). The cooked reading expanded `min` **at the use**, so a `std::min(a, b)`
//! became `std::((((a) < (b)) ? (a) : (b)))` — a `::` followed by a parenthesis — and the file was reported as a syntax
//! error the reader could not fix, at a line whose text a compiler accepts.
//!
//! The `#undef` is in an included header, *before* the use, and the macro is defined in a header that header includes:
//!
//! ```cpp
//! /* defined.h */  #define min(a, b) (((a) < (b)) ? (a) : (b))
//! /* pch.h */      #include "defined.h"
//!                  #undef min
//! /* main.cpp */   #include "pch.h"
//!                  int x = std::min(1, 2);
//! ```

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn a_project(name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let root: PathBuf = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    for (name, text) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory");
        }
        std::fs::write(&path, text).expect("the fixture writes");
    }
    (root.clone(), root.join("main.cpp"))
}

#[test]
fn a_macro_an_included_header_undefines_is_not_expanded_at_the_use() {
    let (root, file) = a_project(
        "cppls-cook-undef",
        &[
            (
                "defined.h",
                "#pragma once\n#define min(a, b) (((a) < (b)) ? (a) : (b))\n",
            ),
            (
                "pch.h",
                "#pragma once\n#include \"defined.h\"\n#undef min\n",
            ),
            (
                "main.cpp",
                "#include \"pch.h\"\n\nstruct Frame { int value; };\n\nint f() {\n    Frame a{};\n    Frame b{};\n\x20   return std::min(a.value, b.value);\n}\n",
            ),
        ],
    );
    let text = std::fs::read_to_string(&file).expect("the fixture reads");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    session.catch_up(&file);
    let _ = session.index_everything();

    let materials = session.cooking_materials(&file).expect("materials");
    let rendered = session.render_a_cooked(&materials).expect("a rendering");
    let cooked = rendered.indexed.rendered.text.clone();
    assert!(
        cooked.contains("std::min"),
        "the use is a call of `std::min`, so the `#undef` reaches it: {}",
        &cooked[cooked.find("std::").unwrap_or(0)..]
    );
    assert!(
        rendered.reading.diagnostics == 0,
        "and the rendering parses: {:?}",
        rendered.indexed.diagnostics
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The reported shape**: both the `#define` and the `#undef` are inside a conditional — the precompiled-header
/// arrangement, where the platform's `min`/`max` are defined under `#ifndef NOMINMAX` and taken away under
/// `#if defined(_WIN32)`.
///
/// This is the case the *unit* path got wrong in a second place: an `#undef` in force was not a "visible binding"
/// (`visible_binding` wanted `unconditional || body.is_some()`, and an `#undef` has neither), and the in-force
/// channel kept the body it had already collected. Measured on the production project after the composed-path fix:
/// the cook still expanded `std::min`, because that file cooks through a **held unit**. Verified there by
/// re-rendering: 2 parse errors before, 0 after, and the `std::min` call left as written.
#[test]
fn a_guarded_undef_takes_away_a_guarded_define() {
    let (root, file) = a_project(
        "cppls-cook-guarded-undef",
        &[
            (
                "defined.h",
                "#pragma once\n#if defined(_WIN32)\n#define min(a, b) (((a) < (b)) ? (a) : (b))\n#endif\n",
            ),
            (
                "pch.h",
                "#pragma once\n#include \"defined.h\"\n#if defined(_WIN32)\n#undef min\n#endif\n",
            ),
            (
                "main.cpp",
                "#include \"pch.h\"\n\nstruct Frame { int value; };\n\nint f() {\n    Frame a{};\n    Frame b{};\n\x20   return std::min(a.value, b.value);\n}\n",
            ),
        ],
    );
    let text = std::fs::read_to_string(&file).expect("the fixture reads");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    session.catch_up(&file);
    let _ = session.index_everything();

    let materials = session.cooking_materials(&file).expect("materials");
    let rendered = session.render_a_cooked(&materials).expect("a rendering");
    let cooked = rendered.indexed.rendered.text.clone();
    assert!(
        cooked.contains("std::min"),
        "the guarded `#undef` takes the guarded `#define` away: {}",
        &cooked[cooked.find("std::").unwrap_or(0)..]
    );
    assert!(
        rendered.reading.diagnostics == 0,
        "and the rendering parses: {:?}",
        rendered.indexed.diagnostics
    );

    let _ = std::fs::remove_dir_all(&root);
}
