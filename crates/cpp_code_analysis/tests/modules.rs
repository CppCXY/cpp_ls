//! The module graph: resolving imports, and being honest when they do not resolve.
//!
//! Two themes run through this file. The first is that **an unresolved import is not an absent one** — a
//! module may be shipped as a prebuilt BMI with no source in the project, which is the normal state of
//! `import std;`, so every failure outcome exists to keep "I could not find it" apart from "it is not
//! there". The second is that the **reverse edge is the point**: a change to a module interface unit has to
//! name the files that must be re-analysed without re-scanning the project.

use cpp_code_analysis::{
    CompilerConfig, FileProvider, ImportEdge, ImportOutcome, ImportTarget, MAX_IMPORT_DEPTH,
    MemoryFiles, ModuleGraph, ModuleUnit, PathInterner, scan_imports,
};
use cpp_parser::{CppParser, ParserConfig};
use std::path::Path;

/// Scan from a named root, with no compiler configuration.
fn scan(files: &MemoryFiles, root: &str) -> (ModuleGraph, PathInterner) {
    scan_with(files, root, &CompilerConfig::new())
}

/// Scan from a named root under a given configuration.
fn scan_with(
    files: &MemoryFiles,
    root: &str,
    config: &CompilerConfig,
) -> (ModuleGraph, PathInterner) {
    let source = files.read(Path::new(root)).expect("the root exists");
    let tree = CppParser::parse(&source, ParserConfig::default());

    let mut interner = PathInterner::new(files.is_case_insensitive());
    let graph = scan_imports(&tree, Path::new(root), files, config, &mut interner);

    (graph, interner)
}

/// The files the scan read, as paths.
fn analysed(graph: &ModuleGraph, interner: &PathInterner) -> Vec<String> {
    graph
        .analysed
        .iter()
        .filter_map(|file| interner.path(*file))
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect()
}

/// The imports of a file, collected so a test can index and measure them.
///
/// The library hands back an iterator; a test almost always wants to assert on the whole list, and collecting
/// in one place keeps that noise out of every case.
fn imports_of<'a>(
    graph: &'a ModuleGraph,
    interner: &PathInterner,
    file: &str,
) -> Vec<&'a cpp_code_analysis::ImportEdge> {
    let id = interner.get(Path::new(file)).unwrap_or_else(|| {
        panic!(
            "{file} is not in the graph; analysed: {:?}",
            analysed(graph, interner)
        )
    });

    graph.imports_of(id).collect()
}

/// An import outcome as a short readable string, so a failure names the state rather than a variant index.
fn outcome_of(graph: &ModuleGraph, file: &str, interner: &PathInterner) -> String {
    let id = interner.get(Path::new(file)).unwrap_or_else(|| {
        panic!(
            "{file} is not in the graph; analysed: {:?}",
            analysed(graph, interner)
        )
    });

    let edge = graph
        .imports_of(id)
        .next()
        .unwrap_or_else(|| panic!("{file} imports nothing; edges: {:?}", graph.edges));

    match &edge.outcome {
        ImportOutcome::Resolved(target) => format!(
            "resolved to {}",
            interner
                .path(*target)
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        ),
        other => other.describe(),
    }
}

// ============================================================================
// The conventional layout
// ============================================================================

/// The layout every build system uses: the interface unit is named after the module.
///
/// `m` → `m.cppm` beside the importing file. This is the cheap path, and it is worth pinning that it works
/// without any project scan: resolution that needed one would be per-keystroke work proportional to the
/// project.
#[test]
fn an_import_resolves_through_the_naming_convention() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import m;\nint main() {}\n")
        .with_file("m.cppm", "export module m;\nexport int f();\n");

    let (graph, interner) = scan(&files, "main.cpp");

    assert_eq!(
        outcome_of(&graph, "main.cpp", &interner),
        "resolved to m.cppm"
    );
    assert_eq!(analysed(&graph, &interner), vec!["main.cpp", "m.cppm"]);
}

/// A dotted module name is looked for as a path: `my.mod` → `my/mod.cppm`.
///
/// The flat spelling `my.mod.cppm` is tried too, because plenty of projects keep modules in one directory —
/// both are proposals confirmed by parsing, so trying both costs a failed read and never a wrong answer.
#[test]
fn a_dotted_module_name_resolves_by_path() {
    for layout in ["my/mod.cppm", "my.mod.cppm"] {
        let files = MemoryFiles::new()
            .with_file("main.cpp", "import my.mod;\n")
            .with_file(layout, "export module my.mod;\n");

        let (graph, interner) = scan(&files, "main.cpp");

        assert_eq!(
            outcome_of(&graph, "main.cpp", &interner),
            format!("resolved to {layout}"),
            "layout {layout:?}"
        );
    }
}

/// A module reached through another module is in the graph too.
///
/// Transitive resolution is what makes the reverse edges complete: `main` imports `a`, `a` imports `b`, and
/// a change to `b` has to reach `main` even though `main` never names it.
#[test]
fn imports_are_followed_transitively() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import a;\n")
        .with_file("a.cppm", "export module a;\nimport b;\nexport int f();\n")
        .with_file("b.cppm", "export module b;\nexport int g();\n");

    let (graph, interner) = scan(&files, "main.cpp");

    assert_eq!(
        analysed(&graph, &interner),
        vec!["main.cpp", "a.cppm", "b.cppm"]
    );
    assert_eq!(graph.units.len(), 2, "two module units: {:?}", graph.units);
}

/// Every unit kind is recorded with the module it belongs to.
///
/// The partition is reached by **importing** it, which is the only way anything reaches a partition: a
/// partition is not a module, so nothing outside `m` can name it, and a scan that is not following `m`'s own
/// imports has no reason to read its files. `import :part` inside `m` is what makes `m:part` part of the
/// graph — and the file is named `m-part.cppm` rather than `m:part.cppm`, which the convention has to
/// translate.
#[test]
fn units_carry_their_module_and_partition() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import m;\n")
        .with_file("m.cppm", "export module m;\nimport :part;\n")
        .with_file("m-part.cppm", "export module m:part;\n");

    let (graph, _) = scan(&files, "main.cpp");

    let names: Vec<String> = graph
        .units
        .iter()
        .map(|unit| format!("{} ({})", unit.qualified_name(), unit.unit.describe()))
        .collect();

    assert_eq!(
        names,
        vec![
            "m (module interface unit)".to_string(),
            "m:part (module partition interface unit)".to_string(),
        ]
    );
}

/// A partition file named after the partition — `m:part.cppm` — is found too.
///
/// Both spellings are in use: some projects write `m-part.cppm`, others keep the colon out of the file name
/// entirely. Nothing in the language says which, so both are tried, and a candidate is confirmed by parsing
/// it rather than trusted.
#[test]
fn a_partition_resolves_under_either_file_naming() {
    for layout in ["m-part.cppm", "m.part.cppm"] {
        let files = MemoryFiles::new()
            .with_file("m.cppm", "export module m;\nimport :part;\n")
            .with_file(layout, "export module m:part;\n");

        let (graph, interner) = scan(&files, "m.cppm");

        assert_eq!(
            outcome_of(&graph, "m.cppm", &interner),
            format!("resolved to {layout}"),
            "layout {layout:?}"
        );
    }
}

/// An implementation unit for a partition is **not** what a partition import resolves to.
///
/// `import :part;` asks for the partition's interface, because that is where its exported names are declared.
/// Resolving to the implementation unit would find a file that does not export what the importer is asking
/// for — the same class of mistake as resolving a name to the wrong unit of a module.
#[test]
fn a_partition_import_resolves_to_the_interface_not_the_implementation() {
    let files = MemoryFiles::new()
        .with_file("m.cppm", "export module m;\nimport :part;\n")
        .with_file("m-part.cppm", "export module m:part;\nexport int f();\n")
        .with_file("m-part-impl.cpp", "module m:part;\nint g();\n");

    let (graph, interner) = scan(&files, "m.cppm");

    assert_eq!(
        outcome_of(&graph, "m.cppm", &interner),
        "resolved to m-part.cppm",
        "the interface unit"
    );

    // The implementation unit is only in the graph if something read it, and nothing here does.
    assert!(
        !analysed(&graph, &interner).contains(&"m-part-impl.cpp".to_string()),
        "the implementation unit was not pulled in: {:?}",
        analysed(&graph, &interner)
    );
}

// ============================================================================
// The reverse edges
// ============================================================================

/// A change to a module interface unit names exactly the files that import it.
///
/// The reverse edge is what the whole graph exists for: without it, a change to `shared.cppm` means
/// discarding every analysis in the project, which is the same as having no cache.
#[test]
fn importers_of_is_the_reverse_edge() {
    let files = MemoryFiles::new()
        .with_file("one.cpp", "import shared;\n")
        .with_file("two.cpp", "import shared;\n")
        .with_file("three.cpp", "int x;\n")
        .with_file("shared.cppm", "export module shared;\nexport int f();\n");

    let mut interner = PathInterner::new(false);
    let mut graphs = Vec::new();

    for unit in ["one.cpp", "two.cpp", "three.cpp"] {
        let source = files.read(Path::new(unit)).expect("the unit");
        let tree = CppParser::parse(&source, ParserConfig::default());
        graphs.push(scan_imports(
            &tree,
            Path::new(unit),
            &files,
            &CompilerConfig::new(),
            &mut interner,
        ));
    }

    // One interner across the scans, which is what makes the three graphs agree on which file `shared.cppm`
    // is: a `FileId` is only an identity if the table that minted it is shared.
    let shared = interner.get(Path::new("shared.cppm")).expect("shared.cppm");

    let affected: std::collections::HashSet<String> = graphs
        .iter()
        .flat_map(|graph| graph.importers_of(shared))
        .filter_map(|file| interner.path(file))
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();

    assert_eq!(
        affected,
        ["one.cpp", "two.cpp"]
            .into_iter()
            .map(String::from)
            .collect(),
        "the third file does not import it"
    );
}

/// `dependents_of` reaches the importers of importers, which is what a cache invalidation needs.
#[test]
fn dependents_of_follows_the_chain() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import a;\n")
        .with_file("a.cppm", "export module a;\nimport b;\n")
        .with_file("b.cppm", "export module b;\nimport c;\n")
        .with_file("c.cppm", "export module c;\n");

    let (graph, interner) = scan(&files, "main.cpp");

    let c = interner.get(Path::new("c.cppm")).expect("c.cppm");
    let mut affected: Vec<String> = graph
        .dependents_of(c)
        .iter()
        .filter_map(|file| interner.path(*file))
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    affected.sort();

    assert_eq!(
        affected,
        vec!["a.cppm", "b.cppm", "main.cpp"],
        "everything above c, transitively"
    );
}

/// A root has no importers, and the query is empty rather than wrong.
///
/// The distinction the name is about: a root *does* import things — that is what makes it a root of this
/// graph — but nothing imports *it*, so the reverse query is empty. Conflating the two directions would make
/// a cache invalidation either discard the root or miss everything below it.
#[test]
fn a_root_has_no_importers() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import m;\n")
        .with_file("m.cppm", "export module m;\n");

    let (graph, interner) = scan(&files, "main.cpp");
    let main = interner.get(Path::new("main.cpp")).expect("main.cpp");

    assert_eq!(
        graph.imports_of(main).count(),
        1,
        "the root is where the dependency starts"
    );
    assert!(graph.importers_of(main).is_empty(), "nothing imports it");
    assert!(graph.dependents_of(main).is_empty());
}

/// A file with no imports at all has an empty forward query, which is the other direction of the same
/// question and worth pinning separately.
#[test]
fn a_file_with_no_imports_has_no_needs() {
    let files = MemoryFiles::new().with_file("main.cpp", "#include <vector>\nint main() {}\n");

    let (graph, interner) = scan(&files, "main.cpp");
    let main = interner.get(Path::new("main.cpp")).expect("main.cpp");

    assert!(graph.imports_of(main).next().is_none());
    assert!(graph.needs_of(main).is_empty());
}

// ============================================================================
// Failure: unresolved is not absent
// ============================================================================

/// A module nothing declares is reported as **unknown**, not as empty or missing.
///
/// `import std;` is the case that makes this matter. Almost no project has a file declaring `std` — it is a
/// prebuilt BMI — so an analysis that treated an unresolved import as "this module has no names" would
/// report every `std::` name as undefined. The outcome says "I could not find it", and
/// `may_provide_names` is false because the names are *unknown*, which is what stops a consumer from
/// concluding they do not exist.
#[test]
fn an_unknown_module_is_unknown_rather_than_absent() {
    let files = MemoryFiles::new().with_file("main.cpp", "import std;\n");

    let (graph, interner) = scan(&files, "main.cpp");

    assert_eq!(
        imports_of(&graph, &interner, "main.cpp")[0].outcome,
        ImportOutcome::UnknownModule { name: "std".into() }
    );

    let unresolved = graph.unresolved_imports();
    assert_eq!(unresolved.len(), 1);
    assert!(
        !unresolved[0].outcome.may_provide_names(),
        "the names are unknown, so nothing may be concluded about them"
    );
    assert!(
        unresolved[0]
            .outcome
            .describe()
            .contains("no file in this project"),
        "and the message says what was actually established: {}",
        unresolved[0].outcome.describe()
    );
}

/// A file that exists under the conventional name but declares a different module is a **mismatch**, not a
/// resolution.
///
/// This is the failure that looks like success. `import m;` finds `m.cppm`, so a resolver that stopped at
/// "the file exists" would record a resolved edge and then attribute whatever `m.cppm` actually declares to
/// module `m` — a wrong answer produced by the very check that was supposed to prevent it.
#[test]
fn a_file_that_declares_something_else_is_a_mismatch() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import m;\n")
        .with_file("m.cppm", "export module other;\n");

    let (graph, interner) = scan(&files, "main.cpp");
    let edge = imports_of(&graph, &interner, "main.cpp")[0];
    let edge = edge.clone();

    assert_eq!(
        edge.outcome,
        ImportOutcome::NotDeclaredHere {
            name: "m".into(),
            tried: std::path::PathBuf::from("m.cppm"),
        }
    );
    assert!(!edge.outcome.may_provide_names());
    assert!(
        edge.outcome.describe().contains("does not declare"),
        "{}",
        edge.outcome.describe()
    );
}

/// A partition that the module does not declare is reported against the module, not as a missing module.
///
/// `import :part;` from inside `m` asks about `m`'s own partitions, so "no partition `:part` in `m`" is a
/// different statement from "no module `part`" — and the two lead to different fixes. Treating the partition
/// as a module is the mistake this pins.
#[test]
fn a_missing_partition_is_reported_against_its_module() {
    let files = MemoryFiles::new().with_file("m.cppm", "export module m;\nimport :missing;\n");

    let (graph, interner) = scan(&files, "m.cppm");
    let edge = imports_of(&graph, &interner, "m.cppm")[0].clone();

    assert_eq!(
        edge.outcome,
        ImportOutcome::UnknownPartition {
            module: "m".into(),
            partition: "missing".into(),
        }
    );
    assert!(
        edge.outcome.describe().contains("declares no partition"),
        "{}",
        edge.outcome.describe()
    );
}

/// A partition import in a file with no module at all cannot be resolved, and says so.
///
/// `import :part;` means "a partition of *my* module", so there is nothing to look it up against in a plain
/// translation unit. The alternative — guessing a module name — would produce a resolution to a module the
/// user never mentioned.
#[test]
fn a_partition_import_without_a_module_is_reported() {
    let files = MemoryFiles::new().with_file("main.cpp", "import :part;\n");

    let (graph, interner) = scan(&files, "main.cpp");
    let edge = imports_of(&graph, &interner, "main.cpp")[0].clone();

    assert_eq!(edge.outcome, ImportOutcome::NoModuleToPartition);
    assert!(!edge.outcome.may_provide_names());
}

/// A header unit is resolved with the include search order, and its failure says what was actually wrong.
///
/// A header unit *is* a header, so reusing the include resolver is not a shortcut — a second implementation
/// of "which `vector` did you mean" would be free to disagree with the compiler's.
///
/// The angle failure here has **nowhere to search**, because no include paths are configured, and the message
/// says so rather than "cannot find vector". The distinction is the actionable part: one means "configure
/// your build", the other means "install the library", and a user cannot tell which from the file.
#[test]
fn a_header_unit_uses_the_include_search_order() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import \"local.h\";\nimport <vector>;\n")
        .with_file("local.h", "int x;\n");

    let (graph, interner) = scan(&files, "main.cpp");
    let edges = imports_of(&graph, &interner, "main.cpp");

    assert_eq!(
        edges[0].outcome,
        ImportOutcome::Resolved(interner.get(Path::new("local.h")).unwrap()),
        "the quoted form found the project header"
    );

    match &edges[1].outcome {
        ImportOutcome::UnknownHeaderUnit { name, searched } => {
            assert_eq!(&**name, "vector");
            assert!(
                searched.is_empty(),
                "an angle import with no include paths has nowhere to look: {searched:?}"
            );
            assert!(
                edges[1]
                    .outcome
                    .describe()
                    .contains("no include directories are configured"),
                "and the message says that rather than blaming the header: {}",
                edges[1].outcome.describe()
            );
        }
        other => panic!("expected an unresolvable header unit, got {other:?}"),
    }
}

/// A partition file that declares the **wrong** partition is a mismatch, not a resolution.
///
/// The same failure as a module name mismatch, one level down: `import :part;` finds `m-part.cppm` by the
/// naming convention, and that file declares `m:other`. Recording a resolved edge would attribute `other`'s
/// exports to `:part`, so the mismatch is reported and the file that was tried is kept for the message.
#[test]
fn a_partition_file_declaring_another_partition_is_a_mismatch() {
    let files = MemoryFiles::new()
        .with_file("m.cppm", "export module m;\nimport :part;\n")
        .with_file("m-part.cppm", "export module m:other;\n");

    let (graph, interner) = scan(&files, "m.cppm");
    let edge = imports_of(&graph, &interner, "m.cppm")[0].clone();

    assert_eq!(
        edge.outcome,
        ImportOutcome::PartitionMismatch {
            module: "m".into(),
            partition: "part".into(),
            tried: std::path::PathBuf::from("m-part.cppm"),
        }
    );
    assert!(!edge.outcome.may_provide_names());
    assert!(
        edge.outcome.describe().contains("is not partition"),
        "{}",
        edge.outcome.describe()
    );
}

/// A partition file that declares no partition at all is also a mismatch.
///
/// `m-part.cppm` declaring plain `module m;` is the mistake a copy-paste makes, and it is the same shape as
/// the case above from the resolver's point of view — which is why one check covers both, and why the state
/// is "not the partition I asked for" rather than "not a partition".
#[test]
fn a_partition_file_without_a_partition_is_a_mismatch() {
    let files = MemoryFiles::new()
        .with_file("m.cppm", "export module m;\nimport :part;\n")
        .with_file("m-part.cppm", "export module m;\n");

    let (graph, interner) = scan(&files, "m.cppm");
    let edge = imports_of(&graph, &interner, "m.cppm")[0].clone();

    assert!(
        matches!(edge.outcome, ImportOutcome::PartitionMismatch { .. }),
        "{:?}",
        edge.outcome
    );
}

/// A header unit found along a configured include path resolves, which is how a standard library supplied as
/// header units reaches a project.
#[test]
fn a_header_unit_resolves_along_an_include_path() {
    let files = MemoryFiles::new()
        .with_file("src/main.cpp", "#include \"local.h\"\nimport <vector>;\n")
        .with_file("include/vector", "int v;\n");

    let config = CompilerConfig::new().with_include_path("include");
    let (graph, interner) = scan_with(&files, "src/main.cpp", &config);
    let edges = imports_of(&graph, &interner, "src/main.cpp");

    assert_eq!(
        edges[0].outcome,
        ImportOutcome::Resolved(interner.get(Path::new("include/vector")).unwrap()),
        "the angle form searched the configured path"
    );
}

/// A module shipped only as a prebuilt BMI — no source anywhere — is the unknown case in its purest form.
///
/// `import std;` is the everyday example, and the reason the outcome cannot be "the module is empty". The
/// BMI exists on the build machine and the analysis has no source for it, so the names it exports are
/// *unknown*: reporting them as absent would flag every `std::` use as an error, and reporting the import as
/// resolved would fabricate a file that does not exist. The analysis says neither.
///
/// The test spells out the distinction a consumer keys on: `may_provide_names` is false because nothing is
/// known, which is a different thing from the names being known to be missing.
#[test]
fn a_module_available_only_as_a_bmi_is_unknown() {
    // A configured project: include paths that exist, a standard library that is not source here. Nothing
    // in `files` declares any of these modules, which is exactly the situation on a real machine.
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "import std;\nimport <vector>;\nimport third_party.lib;\nint main() {}\n",
        )
        .with_file("include/vector", "int v;\n");

    let config = CompilerConfig::new().with_include_path("include");
    let (graph, interner) = scan_with(&files, "main.cpp", &config);
    let edges = imports_of(&graph, &interner, "main.cpp");

    // `std` is a named module with no source: unknown.
    assert_eq!(
        edges[0].outcome,
        ImportOutcome::UnknownModule { name: "std".into() }
    );

    // `<vector>` resolved to the header that *is* here — a header unit is built from a header, so having the
    // header is enough, and this is the case that shows resolution working normally beside the unknown one.
    assert!(
        edges[1].outcome.is_resolved(),
        "the header unit resolved: {:?}",
        edges[1].outcome
    );

    // A third-party module with no source either: the same unknown, not a fabricated resolution.
    assert_eq!(
        edges[2].outcome,
        ImportOutcome::UnknownModule {
            name: "third_party.lib".into()
        }
    );

    // Every unresolved outcome refuses to say the names are absent, and every message says what was
    // established instead.
    for edge in graph.unresolved_imports() {
        assert!(
            !edge.outcome.may_provide_names(),
            "nothing may be concluded about names from an unresolved import"
        );
        assert!(
            !edge.outcome.describe().is_empty(),
            "and the outcome can explain itself"
        );
    }
}

/// An unresolved module contributes nothing to `needs_of`, so a build-order query is not given a phantom
/// dependency.
#[test]
fn an_unresolved_import_is_not_a_dependency() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import present;\nimport absent;\n")
        .with_file("present.cppm", "export module present;\n");

    let (graph, interner) = scan(&files, "main.cpp");
    let main = interner.get(Path::new("main.cpp")).expect("main.cpp");

    let needs: Vec<String> = graph
        .needs_of(main)
        .iter()
        .filter_map(|file| interner.path(*file))
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();

    assert_eq!(
        needs,
        vec!["present.cppm"],
        "only what resolved is a dependency"
    );

    // Both imports are recorded, though — the unresolved one is information, not noise.
    assert_eq!(graph.imports_of(main).count(), 2);
    assert_eq!(graph.unresolved_imports().len(), 1);
}

/// A `.h` file cannot accidentally resolve a module import.
///
/// The candidate search tries several extensions, and a header is not a module unit even when its name
/// matches — so `import util;` must not resolve to `util.h` just because the file is there. Resolving to it
/// would report an import as satisfied by a file that declares no module at all.
#[test]
fn a_header_is_not_a_module() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import util;\n")
        .with_file("util.h", "int helper();\n");

    let (graph, interner) = scan(&files, "main.cpp");
    let edge = imports_of(&graph, &interner, "main.cpp")[0].clone();

    assert!(
        !edge.outcome.is_resolved(),
        "a header is not a module unit: {:?}",
        edge.outcome
    );
    assert!(!analysed(&graph, &interner).contains(&"util.h".to_string()));
}

// ============================================================================
// Cycles, limits, and robustness
// ============================================================================

/// A module cycle terminates, and the edges that make it up are still recorded.
///
/// No `import` cycle is valid C++20, but an editor parses files mid-edit and a project may be broken — and a
/// scan that did not terminate on one would hang the editor. The edges stay because the dependency is real:
/// a consumer that knows about the cycle can report it, one that was not told cannot.
#[test]
fn an_import_cycle_terminates_and_is_recorded() {
    let files = MemoryFiles::new()
        .with_file("a.cppm", "export module a;\nimport b;\n")
        .with_file("b.cppm", "export module b;\nimport a;\n");

    let (graph, interner) = scan(&files, "a.cppm");

    assert_eq!(analysed(&graph, &interner), vec!["a.cppm", "b.cppm"]);
    assert_eq!(graph.edges.len(), 2, "both edges: {:?}", graph.edges);
    assert!(
        graph.edges.iter().all(|edge| edge.outcome.is_resolved()),
        "both resolved — it is the layout that is cyclic, not the resolution"
    );
}

/// A module that imports itself terminates.
#[test]
fn a_self_import_terminates() {
    let files = MemoryFiles::new().with_file("a.cppm", "export module a;\nimport a;\n");

    let (graph, _) = scan(&files, "a.cppm");

    assert_eq!(graph.analysed.len(), 1);
    assert_eq!(graph.edges.len(), 1);
}

/// A diamond is read once, and its own imports are recorded once.
///
/// Without the visited set, `main → a → shared` and `main → b → shared` would duplicate every edge below
/// `shared`, and a consumer counting or walking dependencies would see each twice.
#[test]
fn a_diamond_reads_the_shared_module_once() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import a;\nimport b;\n")
        .with_file("a.cppm", "export module a;\nimport shared;\n")
        .with_file("b.cppm", "export module b;\nimport shared;\n")
        .with_file("shared.cppm", "export module shared;\n");

    let (graph, interner) = scan(&files, "main.cpp");

    let shared_count = analysed(&graph, &interner)
        .iter()
        .filter(|path| *path == "shared.cppm")
        .count();

    assert_eq!(
        shared_count,
        1,
        "read once: {:?}",
        analysed(&graph, &interner)
    );
    assert_eq!(graph.units.len(), 3, "one unit each: {:?}", graph.units);
}

/// A chain deeper than the limit stops rather than running away.
#[test]
fn a_chain_deeper_than_the_limit_stops() {
    let mut files = MemoryFiles::new().with_file("m0.cppm", "export module m0;\nimport m1;\n");

    for depth in 1..MAX_IMPORT_DEPTH + 5 {
        files.insert(
            format!("m{depth}.cppm"),
            format!("export module m{depth};\nimport m{};\n", depth + 1),
        );
    }

    let (graph, _) = scan(&files, "m0.cppm");

    assert!(
        graph.analysed.len() <= MAX_IMPORT_DEPTH + 2,
        "the scan stopped: {} files",
        graph.analysed.len()
    );
}

/// Whatever the tree of files, the scan terminates without panicking.
#[test]
fn the_scan_never_panics() {
    let cases: Vec<MemoryFiles> = vec![
        MemoryFiles::new(),
        MemoryFiles::new().with_file("a.cppm", ""),
        MemoryFiles::new().with_file("a.cppm", "export module a;\nimport a;\n"),
        MemoryFiles::new()
            .with_file("a.cppm", "export module a;\nimport b;\n")
            .with_file("b.cppm", "export module b;\nimport c;\n")
            .with_file("c.cppm", "export module c;\nimport a;\n"),
        MemoryFiles::new().with_file("a.cppm", "export module a;\nimport ;\n"),
        MemoryFiles::new().with_file("a.cppm", "export module ;\n"),
        MemoryFiles::new().with_file("a.cppm", "import;\nimport\n"),
        MemoryFiles::new().with_file("a.cppm", "export module a;\nimport :;\n"),
        MemoryFiles::new().with_file("a.cppm", "module : private;\n"),
        MemoryFiles::new().with_file("a.cppm", "export module a;\nimport m.b.c.d.e.f;\n"),
        MemoryFiles::new().with_file("a.cppm", "export module a;\nimport <;\n"),
        MemoryFiles::new().with_file("a.cppm", "export module a;\nimport \"unterminated\n"),
        MemoryFiles::new().with_file("a.cppm", "export module a;\nimport \u{1F600};\n"),
    ];

    for files in cases {
        let mut interner = PathInterner::new(false);
        let source = files.read(Path::new("a.cppm")).unwrap_or_default();
        let tree = CppParser::parse(&source, ParserConfig::default());

        let graph = scan_imports(
            &tree,
            Path::new("a.cppm"),
            &files,
            &CompilerConfig::new(),
            &mut interner,
        );

        // Touch the queries, so the closure is part of what is tested rather than elided.
        for file in graph.analysed.clone() {
            let _ = graph.importers_of(file);
            let _ = graph.dependents_of(file);
            let _ = graph.needs_of(file);
            let _ = graph.imports_of(file);
        }
        let _ = graph.unresolved_imports();
        let _ = graph.interface_unit("a");
        let _ = graph.partition_unit("a", "b");
        let _ = graph.units_of("a");
    }
}

/// A file with no module declaration is a plain translation unit, and the scan says so rather than inventing
/// a unit for it.
#[test]
fn a_plain_translation_unit_has_no_unit() {
    let files = MemoryFiles::new().with_file("main.cpp", "#include <vector>\nint main() {}\n");

    let (graph, interner) = scan(&files, "main.cpp");
    let main = interner.get(Path::new("main.cpp")).expect("main.cpp");

    assert!(graph.units.is_empty(), "{:?}", graph.units);
    assert!(graph.unit_of(main).is_none());
    assert!(graph.edges.is_empty());
}

// ============================================================================
// The difference from `#include`
// ============================================================================

/// An [`ImportEdge`] carries a target, a resolution, and where it was written — and **nowhere to put a macro
/// environment**.
///
/// The structural half of "an import is not textual inclusion", and the half that cannot regress quietly. The
/// behavioural half — that a module unit's macros are its own, and that the include walk does thread macros
/// where this does not — is in `module_semantics.rs`, where the two mechanisms are contrasted on the same
/// pair of files.
///
/// This test exists because the tempting change is to *add* a field: a `macros` on the edge, threaded like
/// [`FileGraph`]'s. Doing so would decide the imported module's conditions against macros no compiler ever
/// used. Destructuring the struct exhaustively means an added field fails to compile here, which turns a
/// design mistake into a test failure at the moment it is made.
#[test]
fn an_import_edge_has_no_macro_environment() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#define FROM_MAIN 1\nimport body;\n")
        .with_file("body.cppm", "export module body;\n");

    let (modules, interner) = scan(&files, "main.cpp");
    let edge = modules
        .imports_of(interner.get(Path::new("main.cpp")).expect("main.cpp"))
        .next()
        .expect("the import");

    assert!(edge.outcome.is_resolved(), "{:?}", edge.outcome);

    let ImportEdge {
        from: _,
        from_module: _,
        target: _,
        is_reexport: _,
        outcome: _,
        range: _,
    } = edge;

    // The module is read — the scan follows imports transitively, which is how a module reached only through
    // another module ends up in the graph. What it is not read *as* is text: nothing from `main.cpp` is in
    // force inside it, so reading it cannot change what its conditions mean.
    assert!(
        analysed(&modules, &interner).contains(&"body.cppm".to_string()),
        "a resolved import is followed: {:?}",
        analysed(&modules, &interner)
    );
}

/// A re-export is recorded as such, because it changes what the module's own interface contains.
#[test]
fn a_re_export_is_recorded() {
    let files = MemoryFiles::new()
        .with_file(
            "m.cppm",
            "export module m;\nexport import other;\nimport plain;\n",
        )
        .with_file("other.cppm", "export module other;\n")
        .with_file("plain.cppm", "export module plain;\n");

    let (graph, interner) = scan(&files, "m.cppm");
    let edges = imports_of(&graph, &interner, "m.cppm");

    assert_eq!(edges.len(), 2);
    assert!(edges[0].is_reexport, "`export import other;`");
    assert!(!edges[1].is_reexport, "plain `import plain;`");

    assert_eq!(
        edges[0].target,
        ImportTarget::Module("other".into()),
        "and the target is still recorded"
    );
}

/// Two scans of the same project give the same answer, which is what makes a cached result usable.
#[test]
fn the_scan_is_deterministic() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import a;\nimport b;\nimport missing;\n")
        .with_file("a.cppm", "export module a;\nimport shared;\n")
        .with_file("b.cppm", "export module b;\nimport shared;\n")
        .with_file("shared.cppm", "export module shared;\n");

    let (first, interner) = scan(&files, "main.cpp");
    let (second, _) = scan(&files, "main.cpp");

    assert_eq!(
        analysed(&first, &interner),
        analysed(&second, &interner),
        "the same files in the same order"
    );
    assert_eq!(
        first.edges.len(),
        second.edges.len(),
        "and the same number of edges"
    );
    assert_eq!(
        first
            .units
            .iter()
            .map(|u| u.qualified_name())
            .collect::<Vec<_>>(),
        second
            .units
            .iter()
            .map(|u| u.qualified_name())
            .collect::<Vec<_>>()
    );
}

/// The unit entry reports the kind a consumer has to branch on, and only the primary interface unit answers
/// a named import.
#[test]
fn interface_units_are_found_by_module_name() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "import m;\nimport m.part;\n")
        .with_file("m.cppm", "export module m;\nimport :part;\n")
        .with_file("m-impl.cpp", "module m;\nint x;\n")
        .with_file("m-part.cppm", "export module m:part;\n");

    let (graph, interner) = scan(&files, "main.cpp");

    let interface = graph.interface_unit("m").expect("an interface unit");
    assert_eq!(interface.unit, ModuleUnit::InterfaceUnit);
    assert_eq!(
        interface.path.to_string_lossy().replace('\\', "/"),
        "m.cppm",
        "the implementation unit is not the one importers see"
    );

    let all: Vec<String> = graph
        .units_of("m")
        .iter()
        .map(|unit| unit.qualified_name())
        .collect();

    assert!(
        all.contains(&"m:part".to_string()),
        "the partition belongs to `m`: {all:?}"
    );

    // `import m.part;` is a *module* named `m.part`, not the partition `:part` — the dotted name and the
    // partition are different things, and this is the case where conflating them would silently resolve an
    // import to a file the importer never asked for.
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.target == ImportTarget::Module("m.part".into())
                && !edge.outcome.is_resolved()),
        "a dotted name is not a partition: {:?}",
        graph.edges
    );
    let _ = interner;
}
