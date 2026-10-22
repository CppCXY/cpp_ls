//! **A guarded define in a nested file is not seen by the walk, and this is the reproduction.**
//!
//! The fixture is three files and no includes outside itself:
//!
//! ```text
//! base.h      #define BASE 1
//! guarded.h   #include "base.h"
//!             #ifdef BASE
//!             #define INSIDE 1
//!             #endif
//! root.cpp    #include "guarded.h"
//! ```
//!
//! `BASE` is defined before the `#ifdef BASE` that asks about it, so `INSIDE` is defined in `guarded.h` and in
//! everything that includes it. The chain in `src/effect.rs` says so; `TranslationUnit::walk` does not:
//!
//! ```text
//! guarded.h BASE    → defined, body " 1\n"
//! guarded.h INSIDE  → defined: false          ← the guard was decided against a table without BASE
//! root.cpp  INSIDE  → not reached at all
//! ```
//!
//! So the guard is evaluated against something narrower than the translation unit's own state: the name it asks
//! about *is* in force by the line that asks. This test is `#[ignore]`d because it fails on the current code — it is
//! a regression test waiting for its fix, not a passing assertion. `the_chain_agrees_with_the_walk_on_every_file`
//! (in `src/effect.rs`) is the broader comparison that found it, and the three names it disagrees on are all this.
//!
//! Found by the oracle for the effect model, `docs/exact-index-design.md` §七.

use std::collections::HashMap;
use std::path::PathBuf;

use cpp_code_analysis::summary::MacroDefinitions;
use cpp_code_analysis::{CompilerConfig, FileIndexer, MemoryFiles, SummaryKey, TranslationUnit};

fn unit_of(files: &[(&str, &str)], root: &str) -> TranslationUnit {
    let mut provider = MemoryFiles::new();
    for (path, text) in files {
        provider.insert(*path, *text);
    }
    let config = CompilerConfig::default().with_include_path(PathBuf::from("/p"));
    let indexer = FileIndexer::new(&provider, &config);
    let mut summaries: HashMap<PathBuf, cpp_code_analysis::FileSummary> = HashMap::new();
    let mut texts: HashMap<PathBuf, String> = HashMap::new();
    for (path, text) in files {
        let path = PathBuf::from(*path);
        summaries.insert(path.clone(), indexer.index(&path, text, SummaryKey::new(0, 0)));
        texts.insert(path, text.to_string());
    }
    TranslationUnit::walk(
        summaries.get(std::path::Path::new(root)).expect("indexed"),
        |wanted| {
            summaries
                .get(wanted)
                .map(|summary| (summary, texts.get(wanted).map(String::as_str).unwrap_or("")))
        },
        &cpp_code_analysis::include::graph::Marked::default(),
        &mut MacroDefinitions::default(),
        false,
    )
}

#[test]
#[ignore = "the walk decides `#ifdef BASE` false after `#include \"base.h\"` defined it — see the module note"]
fn a_guarded_define_is_in_force_after_the_include_that_defines_its_condition() {
    let unit = unit_of(
        &[
            ("/p/base.h", "#define BASE 1\n"),
            (
                "/p/guarded.h",
                "#include \"base.h\"\n#ifdef BASE\n#define INSIDE 1\n#endif\n",
            ),
            ("/p/root.cpp", "#include \"guarded.h\"\nint x;\n"),
        ],
        "/p/root.cpp",
    );

    // What the condition asks about, asked of the unit itself: defined, and by the line above it.
    let base = unit
        .macro_at(std::path::Path::new("/p/guarded.h"), "BASE", usize::MAX)
        .expect("BASE was reached");
    assert!(base.defined, "the include above the guard defined BASE");

    let inside = unit
        .macro_at(std::path::Path::new("/p/guarded.h"), "INSIDE", usize::MAX)
        .expect("INSIDE is in guarded.h");
    assert!(
        inside.defined,
        "a `#ifdef BASE` after `#include \"base.h\"` is in force: {:?}",
        inside.defined
    );

    let inherited = unit
        .macro_at(std::path::Path::new("/p/root.cpp"), "INSIDE", usize::MAX)
        .expect("root.cpp includes guarded.h");
    assert!(inherited.defined, "and it reaches the file that includes it");
}
