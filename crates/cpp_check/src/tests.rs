//! The tool's own tests — the arithmetic of a position, the walk's file set, and the code filter's resolution.

use crate::report::{position_of, relative};
use crate::rules::codes;
use crate::walk::discover;

/// **A byte offset as a line and column** — the arithmetic every finding's position comes from.
#[test]
fn a_position_is_a_line_and_a_column() {
    let text = "one\ntwo\nthree\n";
    assert_eq!(position_of(text, 0), (1, 1), "the first byte is 1:1");
    assert_eq!(
        position_of(text, 3),
        (1, 4),
        "the newline is the end of line one"
    );
    assert_eq!(
        position_of(text, 4),
        (2, 1),
        "the byte after it is the start of two"
    );
    assert_eq!(
        position_of(text, 8),
        (3, 1),
        "and three starts after the second"
    );
    // **A column past the end is clamped rather than panicking** — a diagnostic can point at the end of the text,
    // and a slicing panic there would lose the whole run rather than one number.
    assert_eq!(
        position_of(text, 999),
        (4, 1),
        "past the end is the empty last line"
    );
}

/// **The walk finds sources and skips what a build does not own** — the file list is the run's subject.
#[test]
fn the_walk_takes_sources_and_skips_build_output() {
    let root = std::env::temp_dir().join(format!("cpp_check_walk_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("a temp dir");
    std::fs::create_dir_all(root.join("build")).expect("a temp dir");
    for (path, text) in [
        ("src/a.cpp", "int a;\n"),
        ("src/b.c", "int b;\n"),
        ("src/c.h", "int c;\n"),
        ("src/d.hpp", "int d;\n"),
        ("build/generated.cpp", "int e;\n"),
    ] {
        std::fs::write(root.join(path), text).expect("a fixture file");
    }

    let sources = discover(&root, false, &[]);
    let named: Vec<String> = sources
        .iter()
        .map(|path| relative(&root, path).replace('\\', "/"))
        .collect();
    assert_eq!(
        named,
        vec!["src/a.cpp", "src/b.c"],
        "sources, in path order, with `build/` left out and headers excluded"
    );

    // **`--headers` adds the headers, and only the headers' extensions** — `c.h` and `d.hpp` are checked on their
    // own, which is the one context a build never gives them.
    let with_headers = discover(&root, true, &[]);
    let named: Vec<String> = with_headers
        .iter()
        .map(|path| relative(&root, path).replace('\\', "/"))
        .collect();
    assert_eq!(
        named,
        vec!["src/a.cpp", "src/b.c", "src/c.h", "src/d.hpp"],
        "every source and header, still without `build/`"
    );

    // **`--exclude` keeps a subtree out by substring**, which is how a project names its vendored code.
    let excluded = discover(&root, false, &["src".to_string()]);
    assert!(excluded.is_empty(), "everything under `src` was excluded");

    let _ = std::fs::remove_dir_all(&root);
}

/// **A rule name is a name the server also knows**, and a name nobody has is an error rather than an empty filter.
#[test]
fn a_code_filter_resolves_against_the_rules() {
    let known = codes(&[
        "unresolved-call-or-receiver".to_string(),
        "use-after-free".to_string(),
    ])
    .expect("both are rules");
    let known = known.expect("a filter");
    assert!(known.contains("unresolved-call-or-receiver"));
    assert!(known.contains("use-after-free"));

    assert_eq!(
        codes(&[]).expect("no filter"),
        None,
        "no names is no filter"
    );

    let unknown = codes(&["undefined-nam".to_string()]);
    assert_eq!(
        unknown.expect_err("a typo"),
        "undefined-nam",
        "the name nobody has is named back"
    );
}
