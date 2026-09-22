//! The semantic difference between `#include` and `import`.
//!
//! This is the file that pins the one thing the module layer must not do. Inclusion is **textual**: the
//! includer's macros are in force inside the header, which is why [`FileGraph`] threads a macro environment
//! along its edges and why one header can be analysed under two different configurations. A module unit is
//! compiled **on its own**: nothing the importer defines reaches it, nothing it defines reaches the importer,
//! and only its exported declarations cross.
//!
//! Getting this wrong is not a missing answer but a wrong one. An import edge that inherited the importer's
//! macro environment would decide the imported module's `#if` conditions against macros no compiler ever
//! used — so the analysis would confidently claim a branch is dead in a build where it is live. Hence every
//! test here asserts a *difference* between the two mechanisms on the same pair of files, rather than
//! asserting the module graph's behaviour alone: the contrast is the fact being pinned.

use cpp_code_analysis::{
    CompilerConfig, FileProvider, MemoryFiles, ModuleInfo, PathInterner, scan_imports, walk,
};
use cpp_parser::{CppParser, ParserConfig};
use std::path::Path;

/// Parse a source string and read its module shape.
fn info(source: &str) -> ModuleInfo {
    let tree = CppParser::parse(source, ParserConfig::default());
    ModuleInfo::from_tree(&tree.get_red_root())
}

/// Walk `#include`s from a root, returning the graph and interner.
fn include_walk(files: &MemoryFiles, root: &str) -> (cpp_code_analysis::FileGraph, PathInterner) {
    let source = files.read(Path::new(root)).expect("the root");
    let tree = CppParser::parse(&source, ParserConfig::default());

    let mut interner = PathInterner::new(files.is_case_insensitive());
    let graph = walk(
        &source,
        &tree,
        Path::new(root),
        files,
        &CompilerConfig::new(),
        &mut interner,
    );

    (graph, interner)
}

/// Scan `import`s from a root, returning the graph and interner.
fn import_scan(files: &MemoryFiles, root: &str) -> (cpp_code_analysis::ModuleGraph, PathInterner) {
    let source = files.read(Path::new(root)).expect("the root");
    let tree = CppParser::parse(&source, ParserConfig::default());

    let mut interner = PathInterner::new(files.is_case_insensitive());
    let graph = scan_imports(
        &tree,
        Path::new(root),
        files,
        &CompilerConfig::new(),
        &mut interner,
    );

    (graph, interner)
}

/// The macros a file was entered with, as sorted names.
fn macros_at(
    graph: &cpp_code_analysis::FileGraph,
    interner: &PathInterner,
    file: &str,
) -> Vec<String> {
    let id = interner
        .get(Path::new(file))
        .unwrap_or_else(|| panic!("{file} was not reached"));

    graph
        .entry(id)
        .unwrap_or_else(|| panic!("{file} has no entry"))
        .macros
        .iter()
        .map(|name| name.to_string())
        .collect()
}

// ============================================================================
// The property itself
// ============================================================================

/// A module unit's macro environment is its own — for every unit kind.
///
/// Asserted over all four kinds rather than one, because the claim is about being a module unit and not about
/// being an interface: an implementation unit is compiled separately too.
#[test]
fn every_module_unit_owns_its_macro_environment() {
    let sources = [
        "export module m;\n",
        "module m;\n",
        "export module m:part;\n",
        "module m:part;\n",
        "module;\n#include <vector>\nexport module m;\n",
        "export module m;\nmodule : private;\n",
    ];

    for source in sources {
        assert!(
            info(source).macros_are_self_contained(),
            "{source:?} is a module unit and so owns its macros"
        );
    }
}

/// A plain translation unit is not a module unit, and the predicate says so rather than claiming its macros
/// are unreliable.
///
/// The distinction is what keeps the predicate usable: a `.cpp` also owns its macro environment, so
/// `false` here must mean "there is no module unit to make the claim about", not "this file's macros may be
/// wrong". A consumer asking whether an import may carry macros asks it of the *imported* file.
#[test]
fn a_plain_translation_unit_is_not_a_module_unit() {
    for source in ["int x;\n", "#include <vector>\n", "import std;\n"] {
        assert!(!info(source).macros_are_self_contained(), "{source:?}");
        assert!(!info(source).is_module_unit(), "{source:?}");
    }
}

// ============================================================================
// The contrast, on the same pair of files
// ============================================================================

/// A macro the includer defines **is** in force inside the header.
///
/// The baseline the next test is measured against: this is textual inclusion working correctly, and it is
/// why [`FileGraph`] has a macro environment on its edges at all.
#[test]
fn an_include_does_carry_the_includers_macros() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#define OPTION 1\n#include \"body.h\"\n#include \"later.h\"\n",
        )
        .with_file("body.h", "#ifdef OPTION\nint body_sees_it;\n#endif\n")
        .with_file("later.h", "int later;\n");

    let (graph, interner) = include_walk(&files, "main.cpp");

    assert_eq!(
        macros_at(&graph, &interner, "body.h"),
        vec!["OPTION".to_string()],
        "the header is entered with the includer's macro"
    );
    assert_eq!(
        macros_at(&graph, &interner, "later.h"),
        vec!["OPTION".to_string()],
        "and the macro outlives the header it was defined for"
    );
}

/// A header's own `#define` **does** reach the includer after the include point.
///
/// The other direction of the same textual rule, and the reason a header can define a feature macro that the
/// includer then tests.
#[test]
fn a_header_define_reaches_the_includer() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#include \"sets.h\"\n#include \"uses.h\"\n")
        .with_file("sets.h", "#define FROM_HEADER 1\n")
        .with_file("uses.h", "int after;\n");

    let (graph, interner) = include_walk(&files, "main.cpp");

    assert_eq!(
        macros_at(&graph, &interner, "uses.h"),
        vec!["FROM_HEADER".to_string()],
        "the definition propagated past the include point"
    );
}

/// A module's macro environment is **not** the importer's, and this is where the two mechanisms part.
///
/// The module file is entered by the scan with nothing from the importer: the import edge carries a
/// resolution and no macro state, so there is nothing for a condition inside the module to be decided
/// against. The contrast with [`an_include_does_carry_the_includers_macros`] is the whole point — the same
/// shape of file, joined the other way, behaves differently because the language says it must.
#[test]
fn an_import_does_not_carry_the_importers_macros() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#define OPTION 1\nimport body;\n")
        .with_file(
            "body.cppm",
            "export module body;\n#ifdef OPTION\nint body_sees_it;\n#endif\n",
        );

    let (modules, interner) = import_scan(&files, "main.cpp");
    let main = interner.get(Path::new("main.cpp")).expect("main.cpp");

    let edge = modules
        .imports_of(main)
        .next()
        .expect("the import was recorded");

    assert!(edge.outcome.is_resolved(), "{:?}", edge.outcome);

    // The module *is* read — the scan follows imports so that a module reached only through another module
    // is still in the graph. What it is not is read *as text*: nothing from `main.cpp` is in force in it.
    let body = interner.get(Path::new("body.cppm")).expect("body.cppm");
    assert!(
        modules.analysed.contains(&body),
        "the resolved module was followed: {:?}",
        modules.analysed
    );

    // The structural half, and the reason this cannot regress quietly: an edge has nowhere to put a macro
    // environment. Adding one would mean changing this destructuring.
    let cpp_code_analysis::ImportEdge {
        from: _,
        from_module: _,
        target: _,
        is_reexport: _,
        outcome: _,
        range: _,
    } = edge;
}

/// The two graphs disagree about what a file's conditions can be decided by, and the disagreement is the
/// design rather than an oversight.
///
/// The strongest form of the claim, on one pair of files differing only in `#include` versus `import`: the
/// include graph has a macro environment for the second file and the module graph has no such notion. A
/// consumer deciding whether to trust an `#if` inside a file has to ask which way the file was reached, and
/// this test is what says so.
#[test]
fn the_two_graphs_answer_the_macro_question_differently() {
    let body = "#ifdef OPTION\nint conditional;\n#endif\n";

    let via_include = MemoryFiles::new()
        .with_file("main.cpp", "#define OPTION 1\n#include \"body.h\"\n")
        .with_file("body.h", body);

    let via_import = MemoryFiles::new()
        .with_file("main.cpp", "#define OPTION 1\nimport body;\n")
        .with_file("body.cppm", format!("export module body;\n{body}"));

    // Inclusion: the header's environment is the includer's, and the graph says exactly what it is.
    let (includes, interner) = include_walk(&via_include, "main.cpp");
    let header = macros_at(&includes, &interner, "body.h");
    assert_eq!(
        header,
        vec!["OPTION".to_string()],
        "the include walk has an answer, and carries it"
    );

    // Import: there is no such answer to give, and the module graph does not invent one.
    let (modules, interner) = import_scan(&via_import, "main.cpp");
    let main = interner.get(Path::new("main.cpp")).expect("main.cpp");

    assert!(
        modules
            .imports_of(main)
            .all(|edge| edge.outcome.is_resolved()),
        "the module resolved: {:?}",
        modules.edges
    );
    assert!(
        modules
            .units
            .iter()
            .all(|unit| unit.module_name.as_ref() == "body"),
        "and what the graph records about the second file is its *module*, not a macro state: {:?}",
        modules.units
    );
}

// ============================================================================
// The global module fragment: the one place both mechanisms appear in one file
// ============================================================================

/// A module unit may open with `module;` and include headers, and those includes **are** textual.
///
/// The case that looks like an exception to the rule and is not: the fragment's `#include`s belong to the
/// global module, so a header included there keeps ordinary include semantics, while the named module below
/// it still stands alone. The two facts coexist in one file, which is exactly why
/// `has_global_fragment` is recorded separately from `macros_are_self_contained`.
#[test]
fn a_global_module_fragment_keeps_ordinary_include_semantics() {
    let files = MemoryFiles::new()
        .with_file(
            "unit.cppm",
            "#define CONFIG 1\nmodule;\n#include \"global.h\"\nexport module m;\nimport dep;\n",
        )
        .with_file(
            "global.h",
            "#ifdef CONFIG\nint header_sees_config;\n#endif\n",
        )
        .with_file("dep.cppm", "export module dep;\n");

    // The file declares a module, so its macro environment is its own...
    let source = files.read(Path::new("unit.cppm")).expect("unit.cppm");
    let shape = info(&source);
    assert!(shape.has_global_fragment);
    assert!(shape.macros_are_self_contained());
    assert_eq!(shape.module_name.as_deref(), Some("m"));

    // ...and the header included in the fragment is still reached textually, with the macro in force.
    let (includes, interner) = include_walk(&files, "unit.cppm");
    let header = macros_at(&includes, &interner, "global.h");
    assert!(
        header.iter().any(|name| name == "CONFIG"),
        "the fragment's `#include` is an ordinary include: {header:?}"
    );

    // The import below the module declaration is the module-shaped edge, and carries no such thing.
    let (modules, interner) = import_scan(&files, "unit.cppm");
    let unit = interner.get(Path::new("unit.cppm")).expect("unit.cppm");
    let edge = modules.imports_of(unit).next().expect("the import");

    assert!(edge.outcome.is_resolved(), "{:?}", edge.outcome);
    assert_eq!(
        edge.from_module.as_deref(),
        Some("m"),
        "and it is attributed to the named module, not to the fragment"
    );
}

/// A module unit with no fragment is not confused with one that has it.
///
/// The two flags decide where a file's textual part ends, so a consumer that read them as one would treat a
/// fragment's headers as the module's own.
#[test]
fn the_fragment_flags_are_independent_of_the_unit_kind() {
    let with_fragment = info("module;\n#include <vector>\nexport module m;\n");
    let without = info("export module m;\n");

    assert!(with_fragment.has_global_fragment);
    assert!(!without.has_global_fragment);

    assert_eq!(with_fragment.unit, without.unit, "both are interface units");
    assert_eq!(with_fragment.module_name, without.module_name);

    assert!(!with_fragment.has_private_fragment);
    assert!(!without.has_private_fragment);

    let private = info("export module m;\nmodule : private;\n");
    assert!(private.has_private_fragment);
    assert!(!private.has_global_fragment);
}

// ============================================================================
// What actually crosses an import
// ============================================================================

/// Only declarations cross an import, and a re-export is the graph's record of that.
///
/// The positive half of "macros do not cross": something does, and the graph has to represent it. A re-export
/// is the case where the crossing is transitive — `export import other;` puts `other`'s declarations into
/// *this* module's interface — so a consumer building an export set follows re-exports and not plain imports.
#[test]
fn only_a_re_export_extends_the_module_interface() {
    let files = MemoryFiles::new()
        .with_file(
            "m.cppm",
            "export module m;\nexport import public_dep;\nimport private_dep;\n",
        )
        .with_file("public_dep.cppm", "export module public_dep;\n")
        .with_file("private_dep.cppm", "export module private_dep;\n");

    let (graph, interner) = import_scan(&files, "m.cppm");
    let m = interner.get(Path::new("m.cppm")).expect("m.cppm");

    let reexported: Vec<String> = graph
        .imports_of(m)
        .filter(|edge| edge.is_reexport)
        .map(|edge| edge.target.describe())
        .collect();

    let private: Vec<String> = graph
        .imports_of(m)
        .filter(|edge| !edge.is_reexport)
        .map(|edge| edge.target.describe())
        .collect();

    assert_eq!(reexported, vec!["module `public_dep`".to_string()]);
    assert_eq!(private, vec!["module `private_dep`".to_string()]);
}
