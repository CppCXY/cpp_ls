//! The module graph: resolving imports, and being honest when they do not resolve.
//!
//! Two themes run through this file. The first is that **an unresolved import is not an absent one** — a
//! module may be shipped as a prebuilt BMI with no source in the project, which is the normal state of
//! `import std;`, so every failure outcome exists to keep "I could not find it" apart from "it is not
//! there". The second is that the **reverse edge is the point**: a change to a module interface unit has to
//! name the files that must be re-analysed without re-scanning the project.

use cpp_code_analysis::{
    CompilerConfig, FileProvider, ImportEdge, ImportOutcome, ImportTarget, MAX_IMPORT_DEPTH,
    MemoryFiles, ModuleGraph, ModuleUnit, PathInterner, Session, scan_imports,
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
/// The BMI exists on the build machine and the analysis has no source for it, so the names it exports are
/// *unknown*: reporting them as absent would flag every `std::` use as an error, and reporting the import as
/// resolved would fabricate a file that does not exist. The analysis says neither.
///
/// The test spells out the distinction a consumer keys on: `may_provide_names` is false because nothing is
/// known, which is a different thing from the names being known to be missing.
#[test]
fn a_module_available_only_as_a_bmi_is_unknown() {
    // A configured project: include paths that exist, a module that is not source here. Nothing in `files`
    // declares any of these modules, which is exactly the situation on a real machine.
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "import std.compat;\nimport <vector>;\nimport third_party.lib;\nint main() {}\n",
        )
        .with_file("include/vector", "int v;\n");

    let config = CompilerConfig::new().with_include_path("include");
    let (graph, interner) = scan_with(&files, "main.cpp", &config);
    let edges = imports_of(&graph, &interner, "main.cpp");

    // A named module with no source: unknown. **`std.compat` rather than `std`**, because a compiler that ships a
    // standard module ships its *source* (`<VC>/Tools/MSVC/<version>/modules/std.ixx`), so `import std;` resolves on
    // a machine with that toolchain installed and this test would then be assert about the machine — see
    // `a_module_whose_interface_unit_is_outside_the_project_is_read_in` for that case, and why it is the good one.
    assert_eq!(
        edges[0].outcome,
        ImportOutcome::UnknownModule {
            name: "std.compat".into()
        }
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

/// **A module's names are found through the interface unit's own directory**, not through whatever directory the
/// analysis happens to be running in.
///
/// Measured on the fixture in `tests/fixtures/modules` (a real C++20 module project, built by
/// `target/build_modules.bat`): `import mathlib;` in `main.cpp` beside `mathlib.ixx` answered "no file in this
/// project declares module `mathlib`" — the naming convention had proposed `mathlib.ixx`, and that spelling was
/// read as a relative path against the *process's* directory. The importing file's directory is joined here, and
/// this test is the shape of that mistake: files named relative to `main.cpp`.
#[test]
fn a_module_beside_the_importing_file_is_found_from_that_file() {
    let files = MemoryFiles::new()
        .with_file("src/main.cpp", "import mathlib;\n")
        .with_file(
            "src/mathlib.ixx",
            "export module mathlib;\nexport int add(int, int);\n",
        );

    let (graph, _interner) = scan(&files, "src/main.cpp");

    let interface = graph
        .interface_unit("mathlib")
        .expect("`mathlib.ixx` is beside the file that imports it");
    assert_eq!(
        interface.path.to_string_lossy().replace('\\', "/"),
        "src/mathlib.ixx",
        "and it was found *there* rather than relative to the process"
    );
}

/// **A header unit is a header**, so the names in it are visible to the file that imports it — `import <vector>;`
/// and `import "local.h";` alike, because the search that finds the header is the include search and the
/// declarations are the header's own.
///
/// The fixture this comes from is `tests/fixtures/modules/header_units.cpp`, built and run by
/// `target/build_header_units.bat` (it prints `42`): both spellings, a quoted header beside the importing file and
/// `<iostream>` from the include path.
#[test]
fn a_header_unit_makes_the_headers_declarations_visible() {
    use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

    let memory = MemoryFiles::new()
        .with_file(
            "local_math.h",
            "#pragma once\ninline int twice(int value) { return value * 2; }\n",
        )
        .with_file(
            "main.cpp",
            "import \"local_math.h\";\nint f() { return twice(21); }\n",
        );

    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        ".",
        providers,
        WatchFilter::new("."),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("local_math.h"),
        std::path::PathBuf::from("main.cpp"),
    ]);
    session.index_everything();

    let found = session.index().definition("twice", Path::new("main.cpp"));

    assert!(
        matches!(found, Known::Yes(_)),
        "`main.cpp` imports the header as a unit, so `twice` is in view: {found:?}"
    );

    let _ = DiskFiles;
}

/// **A declaration that is not exported is not visible to an importer** — the rule the module fixture caught: its
/// interface unit writes `export namespace mathlib { … }` and, outside it, `int hidden_helper(int);`, and offering
/// that second name to a file that says `import mathlib;` is offering something that does not compile.
///
/// The shape that made this subtle is in the grammar: `export int f();` puts the keyword **inside** the declaration
/// and `export namespace n { … }` puts it **beside** the namespace, so a reader that only looked for the first
/// spelling found no exported name at all in the fixture — measured, the completion went from one name too many to
/// none.
#[test]
fn only_exported_names_are_visible_through_an_import() {
    use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

    let memory = MemoryFiles::new()
        .with_file(
            "mathlib.ixx",
            "export module mathlib;\n\
             export namespace mathlib { int add(int, int); }\n\
             namespace mathlib { int hidden_helper(int); }\n",
        )
        .with_file(
            "main.cpp",
            "import mathlib;\nint f() { return mathlib::add(1, 2); }\n",
        );

    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        ".",
        providers,
        WatchFilter::new("."),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("mathlib.ixx"),
        std::path::PathBuf::from("main.cpp"),
    ]);
    session.index_everything();

    let exported = session
        .index()
        .definition("mathlib::add", Path::new("main.cpp"));
    assert!(
        matches!(exported, Known::Yes(_)),
        "`add` is inside an exported namespace: {exported:?}"
    );

    let hidden = session
        .index()
        .definition("mathlib::hidden_helper", Path::new("main.cpp"));
    assert!(
        !matches!(hidden, Known::Yes(_)),
        "`hidden_helper` is written outside every `export`, so an importer may not name it: {hidden:?}"
    );

    let _ = DiskFiles;
}

/// **A module whose interface unit is outside the project** — `import std;`, the C++23 spelling — is found through
/// the include paths' sibling `modules` directory, and once found it answers for the names it exports.
///
/// This is the shape MSVC ships: `import std;` is not in the project and not in an include path, it is
/// `<VC>/Tools/MSVC/<version>/modules/std.ixx` — **beside** the include directory — and the file it names is
/// `export module std;` followed by an `#include` of every standard header. So there is nothing exotic to read: the
/// declarations are in the headers, and a file the project never scanned is the only reason `std::string` used to
/// answer `None` from a file that says `import std;`.
///
/// The layout here is the machine's, not a convention invented for the test: `lib/include` is an include path,
/// `lib/modules/std.ixx` is its sibling, and the importing file is in neither directory.
#[test]
fn a_module_whose_interface_unit_is_outside_the_project_is_read_in() {
    use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

    let files = MemoryFiles::new()
        .with_file(
            "lib/modules/std.ixx",
            "export module std;\n#include <string>\n",
        )
        .with_file("lib/include/string", "namespace std { class string {}; }\n")
        .with_file("src/main.cpp", "import std;\nstd::string greeting;\n");

    let config = CompilerConfig::new().with_include_path("lib/include");
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(".", providers, WatchFilter::new("."), config);
    session.add_project_files([std::path::PathBuf::from("src/main.cpp")]);
    session.index_everything();

    // Before the read: the import resolves to a file the index has never seen, so `std::string` is unknown. This is
    // the state the test exists to change, and asserting it first is what makes the second assertion mean something
    // — a `Yes` here would say the answer came from somewhere other than the module.
    let before = session
        .index()
        .definition("std::string", Path::new("src/main.cpp"));
    assert!(
        !matches!(before, Known::Yes(_)),
        "no summary of `lib/modules/std.ixx` exists yet, so nothing can answer for `std`: {before:?}"
    );

    let read_in = session.read_the_modules_a_file_imports(Path::new("src/main.cpp"));
    assert_eq!(
        read_in, 1,
        "one interface unit: `std.ixx`, and nothing else"
    );

    assert_eq!(
        session.index().interface_unit_of("std"),
        Some("lib/modules/std.ixx"),
        "and the index now knows which file an `import std;` reaches"
    );

    let after = session
        .index()
        .definition("std::string", Path::new("src/main.cpp"));
    assert!(
        matches!(after, Known::Yes(_)),
        "`std::string` is declared in the header the interface unit includes: {after:?}"
    );

    // Idempotent, and that is the property the request-driven design rests on: a query asks on every keystroke, and
    // the second ask must cost nothing rather than re-read the standard library.
    assert_eq!(
        session.read_the_modules_a_file_imports(Path::new("src/main.cpp")),
        0,
        "an interface unit already in the index is not read again"
    );

    let _ = DiskFiles;
}

/// **The module names are offered after `import`** — the completion the keyword had none of.
///
/// `import math;` names a file, and a name that is not a declaration is answered by no query the completion had: the
/// scope walk sees nothing, the index by name sees nothing (a module name is not a `DeclFact`), so the list after
/// `import ` was the names in scope — the reader's locals and every macro of every header — and the module's own
/// name had to be typed from memory.
///
/// Two sources, and neither parses: the index (`interface_unit_of`, exact) and the project's own `.cppm`/`.ixx`
/// files, which are named by the convention the resolver searches with. A `.cpp` is **not** one of them even though
/// it may declare a module: measured on `src/`, taking every interface-unit extension offered `main` for
/// `main.cpp` and `math` three times for a project with `math.cpp` and `math.cppm`.
#[test]
fn a_module_name_is_offered_after_import() {
    use cpp_code_analysis::file::coordinates::FileOffset;
    use cpp_code_analysis::{OpenDocuments, SessionFiles, WatchFilter};

    let text = "import \nimport sh\nint x = 0;\n";
    let files = MemoryFiles::new()
        .with_file("src/main.cpp", text)
        .with_file("src/math.cppm", "export module math;\n")
        .with_file("src/shapes.cppm", "export module shapes;\n")
        .with_file("src/math.cpp", "module math;\n");
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session =
        Session::with_config(".", providers, WatchFilter::new("."), CompilerConfig::new());
    session.add_project_files([
        std::path::PathBuf::from("src/main.cpp"),
        std::path::PathBuf::from("src/math.cppm"),
        std::path::PathBuf::from("src/shapes.cppm"),
        std::path::PathBuf::from("src/math.cpp"),
    ]);
    session.index_everything();

    let view = session
        .view_of_the_file("src/main.cpp")
        .expect("the file is held");
    let cursor = text.find("import ").expect("the fixture writes it") + "import ".len();

    let offered = session.completions(&view, FileOffset::new(cursor));
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert!(
        labels.contains(&"math") && labels.contains(&"shapes"),
        "the two modules this project declares: {labels:?}"
    );
    assert!(
        !labels.contains(&"main"),
        "`main.cpp` is a source, not a module unit: {labels:?}"
    );
    assert_eq!(
        labels.iter().filter(|label| **label == "math").count(),
        1,
        "one row per name, however many files mention it: {labels:?}"
    );

    // **The partial word is a prefix, not a new question**: `import sh` narrows to `shapes`, and the range a client
    // replaces is the word the reader typed.
    let partial = text.find("import sh").expect("…") + "import sh".len();
    let narrowed = session.completions(&view, FileOffset::new(partial));
    let labels: Vec<&str> = narrowed
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert_eq!(labels, ["shapes"], "the prefix filters the list");
    assert_eq!(
        narrowed.replace,
        cpp_parser::SourceRange::new(partial - 2, 2),
        "and the client replaces the typed word"
    );
}

/// **`import std;` on GCC — the standard module is a source file in the include tree.**
///
/// A default GCC install ships no `std.gcm`, and the compiler is explicit about it:
///
/// ```text
/// $ c++ -std=c++23 -fmodules-ts -fsyntax-only std_module_demo.cpp
/// std: error: failed to read compiled module: No such file or directory
/// std: note: compiled module file is 'gcm.cache/std.gcm'
/// ```
///
/// What it *does* ship is the module's **interface unit as a source file**: `<include>/bits/std.cc`, 97 KB of
/// `module; #include <bits/stdc++.h> … export module std; export namespace std { … }`, with `bits/std.compat.cc`
/// beside it (verified on MinGW GCC 15.1). That is the file the module is built *from*, so it is the thing to
/// read: the analysis resolves `std` to it, reads it like any other interface unit, and the importing file gets
/// the exported names — no compiler run, no binary module file.
#[test]
fn the_standard_module_is_found_in_gcc_include_tree() {
    use cpp_code_analysis::{OpenDocuments, SessionFiles, WatchFilter};

    let std_cc = "module;\n\
                  #include <bits/stdc++.h>\n\
                  export module std;\n\
                  export namespace std {\n\
                  using size_t = unsigned long long;\n\
                  template <class T> class vector {};\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/inc/bits/std.cc", std_cc)
        .with_file("/inc/bits/stdc++.h", "namespace std { }\n")
        .with_file(
            "/p/std_demo.cpp",
            "import std;\nint main() { std::vector<int> words; return 0; }\n",
        );

    let mut config = CompilerConfig::new();
    config.include_paths = vec![cpp_code_analysis::IncludePath::system("/inc")];
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), config);
    session.add_project_files([std::path::PathBuf::from("/p/std_demo.cpp")]);
    session.index_everything();

    assert!(
        session.imports_a_module_that_is_not_read(std::path::Path::new("/p/std_demo.cpp")),
        "the module is not in the index yet — the state a request finds when it arrives first"
    );
    assert_eq!(
        session.read_the_modules_a_file_imports(std::path::Path::new("/p/std_demo.cpp")),
        1,
        "the request path resolves `std` and reads it in"
    );
    assert_eq!(
        session.index().interface_unit_of("std"),
        Some("/inc/bits/std.cc"),
        "`std` resolves to libstdc++'s own interface unit"
    );

    let found = session
        .index()
        .definitions("std::vector", std::path::Path::new("/p/std_demo.cpp"));
    assert!(
        matches!(found, cpp_code_analysis::Known::Yes(_)),
        "so a name the module exports is in the index for the importer: {found:?}"
    );
}

/// **`export namespace math { … }` exports the namespace's own name, and its concepts.**
///
/// The export region is stored **per declaration shape**, and `exported_at` is answered from the shapes containing
/// an offset. A namespace and a concept declare no specifiers and own no declarator, so neither used to be a shape at
/// all — and a declaration that is not a shape has no exported flag to read:
///
/// ```text
/// export namespace math {
///     template <typename T> concept Number = …;
///     int add(int, int);            ← exported, because a declarator is a shape
/// }
/// ```
///
/// Measured on a real C++23 module project: `add`, `Vec2` and `divide` were `exported: true` while `math` and
/// `Number` were `false`. The consequence is the one a reader sees as "the module is not in my completion": an
/// importing file only receives **exported** facts from a module unit, so `math` was in the index and invisible
/// there, and `math::add` could not be typed without it.
#[test]
fn an_exported_namespace_and_its_concept_are_exported_facts() {
    use cpp_code_analysis::file::coordinates::FileOffset;
    use cpp_code_analysis::{OpenDocuments, SessionFiles, WatchFilter};

    let unit = "export module math;\n\
                export namespace math {\n\
                template <typename T> concept Number = true;\n\
                int add(int lhs, int rhs);\n\
                }\n";
    let caller = "import math;\nint value = mat;\n";
    let files = MemoryFiles::new()
        .with_file("src/math.cppm", unit)
        .with_file("src/main.cpp", caller);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session =
        Session::with_config(".", providers, WatchFilter::new("."), CompilerConfig::new());
    session.add_project_files([
        std::path::PathBuf::from("src/math.cppm"),
        std::path::PathBuf::from("src/main.cpp"),
    ]);
    session.index_everything();

    let summary = session
        .index()
        .summary(std::path::Path::new("src/math.cppm"))
        .expect("the unit is indexed");
    let exported = |name: &str| {
        summary
            .declarations
            .iter()
            .find(|fact| fact.name == name)
            .map(|fact| fact.exported)
    };
    assert_eq!(exported("math"), Some(true), "the namespace's own name");
    assert_eq!(exported("Number"), Some(true), "and a concept inside it");
    assert_eq!(
        exported("add"),
        Some(true),
        "the function, which always was"
    );

    // **And the consequence the reader sees**: the name is offered in the importing file.
    let view = session
        .view_of_the_file("src/main.cpp")
        .expect("the caller is held");
    let cursor = caller.find("mat").expect("the fixture writes it") + "mat".len();
    let offered = session.completions(&view, FileOffset::new(cursor));
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert!(
        labels.contains(&"math"),
        "`math` is a name an importer can write: {labels:?}"
    );
}

/// **A module beside the file that imports it** — the ordinary project layout, and the one the request path missed.
///
/// The two tests above put the interface unit in `lib/modules/`, which is where a *standard* module lives and which
/// the search finds through an include path's sibling `modules/` directory. A project's own modules are beside the
/// file that imports them — `src/main.cpp` says `import math;`, `src/math.cppm` declares it — and that path is found
/// by joining the name onto **the importing file's directory**.
///
/// `Session::module_interface_units_of` passed the importing **file**, so every candidate was
/// `src/main.cpp/math.cppm`: no filesystem has that path, the resolution answered "no file declares `math`", and
/// `read_the_modules_a_file_imports` reported **0** while `imports_a_module_that_is_not_read` stayed true for ever —
/// which made every request in a module file take the session's write lock to discover that there was nothing to
/// read. The fixture has no include paths at all, which is what makes it the one that catches this: with an include
/// path present the sibling `modules/` directory is searched too and the broken first entry is never the one that
/// answers.
#[test]
fn a_module_beside_the_importing_file_is_read_in() {
    use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

    let files = MemoryFiles::new()
        .with_file(
            "src/math.cppm",
            "export module math;\nexport int add(int lhs, int rhs);\n",
        )
        .with_file(
            "src/impl.cpp",
            "module math;\nint add(int lhs, int rhs) { return lhs + rhs; }\n",
        )
        .with_file("src/main.cpp", "import math;\nint x = add(1, 2);\n");

    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session =
        Session::with_config(".", providers, WatchFilter::new("."), CompilerConfig::new());
    session.add_project_files([std::path::PathBuf::from("src/main.cpp")]);
    session.index_everything();

    assert!(
        session.imports_a_module_that_is_not_read(Path::new("src/main.cpp")),
        "the module is not in the index yet — the state a request finds when it arrives first"
    );
    assert_eq!(
        session.read_the_modules_a_file_imports(Path::new("src/main.cpp")),
        1,
        "the interface unit beside the importing file is read in"
    );
    assert_eq!(
        session.index().interface_unit_of("math"),
        Some("src/math.cppm"),
        "and the index now knows where `import math;` goes"
    );
    assert!(
        !session.imports_a_module_that_is_not_read(Path::new("src/main.cpp")),
        "so the next request does not take the write lock to ask again"
    );
    let found = session
        .index()
        .definitions("add", Path::new("src/main.cpp"));
    assert!(
        matches!(found, Known::Yes(_)),
        "and the name the module exports is now in the index: {found:?}"
    );

    let _ = (DiskFiles, OpenDocuments::new());
}

/// The test above stops at the query layer, where a name is a name. This one is the thing a user sees: a cursor after
/// `std::` in a file whose only import is the standard module, and the list that comes back. Both halves have to be
/// right and they are different halves — the module read is what puts the standard library's headers in the index,
/// and the qualified-name query is what walks them.
///
/// The layout is the machine's again (`lib/include` is an include path, `lib/modules/std.ixx` is its sibling), and
/// the module is a *sketch* of MSVC's: `export module std;` followed by the headers that hold the names. That is not
/// a simplification for the test's sake — MSVC's `std.ixx` is 3 194 bytes and every one of them is a declaration of
/// the module and an `#include` line, with no declarations of its own.
#[test]
fn a_names_imported_from_the_standard_module_are_offered_after_the_scope() {
    use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

    let main = "import std;\n\nstd::string greeting{\"hi\"};\n";
    let files = MemoryFiles::new()
        .with_file(
            "lib/modules/std.ixx",
            "export module std;\n#include <string>\n#include <vector>\n",
        )
        .with_file("lib/include/string", "namespace std { class string {}; }\n")
        .with_file(
            "lib/include/vector",
            "namespace std { template <class T> class vector {}; }\n",
        )
        .with_file("src/main.cpp", main);

    let config = CompilerConfig::new().with_include_path("lib/include");
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(".", providers, WatchFilter::new("."), config);
    session.add_project_files([std::path::PathBuf::from("src/main.cpp")]);
    session.want_cooked_reading(std::path::Path::new("src/main.cpp"));
    session.index_everything();

    // The read is the step under test, so it is asked for by name rather than left to a handler.
    assert_eq!(
        session.read_the_modules_a_file_imports(Path::new("src/main.cpp")),
        1,
        "the standard module's interface unit is read in"
    );

    let view = session.view("src/main.cpp").expect("the file is held");
    // The cursor the completion is asked about: **after `std::` and before the name**, which is where a reader is
    // when the popup appears. The offset is found from the text rather than written as a number, because a fixture
    // edited above must not silently move the question.
    let after_the_scope = main.find("std::").expect("the fixture writes `std::`") + "std::".len();
    let found = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(after_the_scope),
    );

    let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();

    assert_eq!(
        found.scope, "std",
        "the answer is the names of that scope, and saying which scope it listed is part of the answer"
    );
    assert!(
        labels.contains(&"string") && labels.contains(&"vector"),
        "both headers `std.ixx` includes contribute their names: {labels:?}"
    );

    let _ = DiskFiles;
}

/// **What a reader is told when an import names a module nothing in the project declares** — the note, on the line
/// that wrote it.
///
/// This is the other half of the pair that makes `import std;` honest. Reading the standard module in is what makes
/// its names answerable (the tests above); this is what happens when it *cannot* be read — a module the compiler
/// keeps outside the project and nobody has built — where the failure mode this project keeps having to remove is
/// silence. The note is not an error and not a warning: nothing is wrong with the file.
///
/// The range is asserted, not just the text: a note about an import is worth nothing if it is not on the line the
/// import is on, and that is the half that needs the file's tree rather than its summary.
#[test]
fn an_import_nothing_declares_gets_a_note_on_the_line_that_wrote_it() {
    use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

    let source = "// a file whose module is not in this project\nimport mylib;\n\nint main() { return 0; }\n";
    let files = MemoryFiles::new().with_file("src/main.cpp", source);

    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session =
        Session::with_config(".", providers, WatchFilter::new("."), CompilerConfig::new());
    session.add_project_files([std::path::PathBuf::from("src/main.cpp")]);
    // **The ask, and then the pump**: `Session::view` is a request for the cooked reading and answers `None` until
    // something has asked for it — the pair every other fixture in this workspace uses.
    session.want_cooked_reading(std::path::Path::new("src/main.cpp"));
    session.index_everything();

    let view = session.view("src/main.cpp").expect("the file is held");
    let notes = session.notes_about_the_modules(&view);

    assert_eq!(notes.len(), 1, "one import, one note: {notes:?}");

    let note = &notes[0];
    assert!(
        note.message.contains("mylib"),
        "the note names the module: {}",
        note.message
    );
    assert!(
        note.message.contains("unknown rather than absent"),
        "and never claims its names do not exist: {}",
        note.message
    );

    // The range covers the `import mylib;` declaration: it starts at the keyword (the note is *about* that line) and
    // it does not run past the declaration — the node's range carries the blank line after it, which is the parser's
    // statement about trivia rather than a second declaration, so the assertion is on where it *starts* and on what
    // it contains.
    let line = "import mylib;";
    let start = source.find(line).expect("the fixture writes it");
    assert_eq!(note.start, start, "the note starts at the `import`");
    assert!(
        source[note.start..note.end].starts_with(line),
        "and the range covers the declaration: {:?}",
        &source[note.start..note.end]
    );
    assert!(
        !source[note.start..note.end].contains("int main"),
        "and stops before the next declaration: {:?}",
        &source[note.start..note.end]
    );

    // A module the project **does** declare gets no note: the reader can go to it, and telling them where their
    // compiler keeps modules would answer a question they did not ask.
    let files = MemoryFiles::new()
        .with_file("src/main.cpp", "import mylib;\n")
        .with_file("src/mylib.ixx", "export module mylib;\n");
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session =
        Session::with_config(".", providers, WatchFilter::new("."), CompilerConfig::new());
    session.add_project_files([
        std::path::PathBuf::from("src/main.cpp"),
        std::path::PathBuf::from("src/mylib.ixx"),
    ]);
    // **The ask, and then the pump** — see the note on the fixture above.
    session.want_cooked_reading(std::path::Path::new("src/main.cpp"));
    session.index_everything();

    let view = session.view("src/main.cpp").expect("the file is held");
    assert!(
        session.notes_about_the_modules(&view).is_empty(),
        "a module this project declares is not a note"
    );

    let _ = DiskFiles;
}

/// **A module written in partitions** — the shape `tests/fixtures/modules/partitions.cpp` is built from, by the real
/// compiler (`target/build_partitions.bat`, which prints `14 12 16`).
///
/// A partition is not a module: `export import :area;` is only legal inside `shapes`, and nothing outside may name
/// it. What it *is* is part of the module's interface — an `export import` re-exports what the partition exports —
/// so a reader of a file that says `import shapes;` must see the partition's names. Measured on the fixture before
/// this was wired: `definition("perimeter")` resolved (declared in the interface unit itself) while `rectangle` and
/// `square`, declared in the partition and re-exported, answered `NotDeclaredHere`.
///
/// The file names are the convention MSVC uses for the same fixture (`shapes-area.cppm` for `shapes:area`), so the
/// naming resolution is exercised as well as the graph.
#[test]
fn a_partitions_names_reach_a_file_that_imports_the_module() {
    use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

    let files = MemoryFiles::new()
        .with_file(
            "shapes.cppm",
            "export module shapes;\n\
             export import :area;\n\
             export int perimeter(int, int);\n",
        )
        .with_file(
            "shapes-area.cppm",
            "export module shapes:area;\n\
             export int rectangle(int, int);\n\
             export int square(int);\n",
        )
        .with_file(
            "partitions.cpp",
            "import shapes;\nint area = rectangle(3, 4);\n",
        );

    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session =
        Session::with_config(".", providers, WatchFilter::new("."), CompilerConfig::new());
    session.add_project_files([
        std::path::PathBuf::from("shapes.cppm"),
        std::path::PathBuf::from("shapes-area.cppm"),
        std::path::PathBuf::from("partitions.cpp"),
    ]);
    session.index_everything();

    // The file that says `import shapes;` sees the interface unit's own name…
    let perimeter = session
        .index()
        .definition("perimeter", Path::new("partitions.cpp"));
    assert!(
        matches!(perimeter, Known::Yes(_)),
        "`perimeter` is declared in the interface unit itself: {perimeter:?}"
    );

    // …and the partition's names, which are in another file that only the module's re-export reaches.
    for name in ["rectangle", "square"] {
        let found = session
            .index()
            .definition(name, Path::new("partitions.cpp"));

        assert!(
            matches!(found, Known::Yes(_)),
            "`{name}` is declared in `shapes:area`, which `shapes.cppm` re-exports: {found:?}"
        );
    }

    let _ = DiskFiles;
}

/// **The reader is told about a partition the project does not have** — and told the right thing.
///
/// The failure this covers is the one a partition import looks like from the analysis's side: `import :area;` asks
/// for a *module* named `area` by every reading that treats a partition as a module, nothing declares one, and the
/// note that followed said "no file declares module `area`" — a sentence about a module that does not exist in the
/// language. Measured on the partition fixture with the partition's file removed: the note has to name the module
/// the partition belongs to and the switch MSVC actually wants (`/reference shapes:area=…`, C7621 without it).
///
/// Both directions in one test, because a note that is always emitted is as wrong as one that never is: the same
/// fixture **with** the partition file present must produce no note at all.
#[test]
fn a_partition_the_project_does_not_have_gets_a_note_about_its_module() {
    use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

    let interface = "export module shapes;\nexport import :area;\n";

    // The partition's file is missing: the analysis knows the module `shapes` — and nothing declares `:area`.
    let files = MemoryFiles::new().with_file("src/shapes.cppm", interface);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session =
        Session::with_config(".", providers, WatchFilter::new("."), CompilerConfig::new());
    session.add_project_files([std::path::PathBuf::from("src/shapes.cppm")]);
    // **The ask, and then the pump** — see the note on the fixture above.
    session.want_cooked_reading(std::path::Path::new("src/shapes.cppm"));
    session.index_everything();

    let view = session.view("src/shapes.cppm").expect("the file is held");
    let notes = session.notes_about_the_modules(&view);

    assert_eq!(notes.len(), 1, "one partition import, one note: {notes:?}");
    assert!(
        notes[0].message.contains(":area") && notes[0].message.contains("`shapes`"),
        "the note names the partition and the module it belongs to: {}",
        notes[0].message
    );
    assert!(
        !notes[0].message.contains("declares module `area`"),
        "and never reads a partition as a module: {}",
        notes[0].message
    );

    // The same file with the partition present: nothing to say.
    let files = MemoryFiles::new()
        .with_file("src/shapes.cppm", interface)
        .with_file(
            "src/shapes-area.cppm",
            "export module shapes:area;\nexport int rectangle(int, int);\n",
        );
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session =
        Session::with_config(".", providers, WatchFilter::new("."), CompilerConfig::new());
    session.add_project_files([
        std::path::PathBuf::from("src/shapes.cppm"),
        std::path::PathBuf::from("src/shapes-area.cppm"),
    ]);
    session.want_cooked_reading(std::path::Path::new("src/shapes.cppm"));
    session.index_everything();

    let view = session.view("src/shapes.cppm").expect("the file is held");
    assert!(
        session.notes_about_the_modules(&view).is_empty(),
        "the partition's file is in the project, so there is nothing to be told"
    );

    let _ = DiskFiles;
}

#[test]
fn an_import_makes_the_modules_names_visible() {
    use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

    let mut memory = MemoryFiles::new()
        .with_file(
            "mathlib.ixx",
            "export module mathlib;\nexport namespace mathlib { int add(int, int); }\n",
        )
        .with_file(
            "main.cpp",
            "import mathlib;\nint f() { return mathlib::add(1, 2); }\n",
        );

    memory = memory.with_file("unrelated.h", "int elsewhere;\n");

    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        ".",
        providers,
        WatchFilter::new("."),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("mathlib.ixx"),
        std::path::PathBuf::from("main.cpp"),
    ]);
    session.index_everything();

    let found = session
        .index()
        .definition("mathlib::add", Path::new("main.cpp"));

    assert!(
        matches!(found, Known::Yes(_)),
        "`main.cpp` says `import mathlib;`, so what the module exports is in view: {found:?}"
    );

    let _ = DiskFiles;
}

/// **GCC's standard module is a source file inside the include tree** — `<c++ include>/bits/std.cc`.
///
/// MSVC ships `modules/std.ixx` beside its include directory; libstdc++ ships the module *source* as
/// `bits/std.cc` **inside** it, and nothing in the project includes that file, so without this read `import std;`
/// resolves to nothing and every name it exports is unknown.
///
/// The layout is the machine's (`lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/bits/std.cc` on this one): the
/// include path is the `c++` root, `bits/` is inside it, and the importing file is in another directory altogether.
/// The file's own shape is the real one — a global module fragment, `export module std;`, then exported names.
#[test]
fn the_gcc_standard_module_is_found_at_bits_std_cc() {
    use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

    let files = MemoryFiles::new()
        .with_file(
            "gcc/include/c++/bits/std.cc",
            "module;\n#define _GLIBCXX_USE_TBB_PAR_BACKEND 0\n\
             export module std;\n\n\
             export namespace std { class string {}; }\n\
             export namespace std::inline literals::inline chrono_literals { int size(); }\n",
        )
        .with_file("src/main.cpp", "import std;\nstd::string greeting;\n");

    let config = CompilerConfig::new().with_include_path("gcc/include/c++");
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(".", providers, WatchFilter::new("."), config);
    session.add_project_files([std::path::PathBuf::from("src/main.cpp")]);
    session.index_everything();

    assert!(
        !matches!(
            session
                .index()
                .definition("std::string", Path::new("src/main.cpp")),
            Known::Yes(_)
        ),
        "before the read, nothing has seen `bits/std.cc`"
    );

    assert_eq!(
        session.read_the_modules_a_file_imports(Path::new("src/main.cpp")),
        1,
        "one interface unit: the module source"
    );

    assert_eq!(
        session.index().interface_unit_of("std"),
        Some("gcc/include/c++/bits/std.cc"),
        "and the index now knows which file `import std;` reaches"
    );

    assert!(
        matches!(
            session
                .index()
                .definition("std::string", Path::new("src/main.cpp")),
            Known::Yes(_)
        ),
        "`std::string` is declared inside the exported namespace the module source writes"
    );

    assert_eq!(
        session.read_the_modules_a_file_imports(Path::new("src/main.cpp")),
        0,
        "the second ask costs nothing — the property the request-driven design rests on"
    );

    let _ = DiskFiles;
}
/// **The pump's half of the same story**: the interface unit is *queued*, not read, so a long closure is paid for in
/// slices rather than in one write hold.
///
/// [`Session::read_the_modules_a_file_imports`] reads the whole closure before it returns — the right trade for a
/// request that is already waiting on an answer, and the wrong one for the pump (whose write holds are measured in
/// milliseconds) or for a *diagnostic*, which is a read and must not add files to the index at all.
#[test]
fn the_interface_unit_of_an_imported_module_can_be_queued_instead_of_read() {
    use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

    let files = MemoryFiles::new()
        .with_file(
            "gcc/include/c++/bits/std.cc",
            "module;\nexport module std;\nexport namespace std { class string {}; }\n",
        )
        .with_file("src/main.cpp", "import std;\nstd::string greeting;\n");

    let config = CompilerConfig::new().with_include_path("gcc/include/c++");
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(".", providers, WatchFilter::new("."), config);
    session.add_project_files([std::path::PathBuf::from("src/main.cpp")]);
    session.index_everything();

    // The pump's question is about **open** documents — the ones a reader is looking at and whose imports are worth
    // paying for — so nothing is queued until the file is open.
    assert_eq!(
        session.queue_the_modules_the_open_files_import(),
        0,
        "no open document imports anything yet"
    );

    session.did_open(
        std::path::Path::new("src/main.cpp"),
        "import std;\nstd::string greeting;\n",
    );
    // The pump asks after a drain, so the file has a summary again by then — an edit drops it, and a summary is what
    // names the modules a file imports.
    session.index_everything();

    assert_eq!(
        session.queue_the_modules_the_open_files_import(),
        1,
        "the open file's `import std;` names one interface unit"
    );
    assert!(
        session.pending_work() > 0,
        "queued rather than read: the unit is work for the pump, not a write hold taken here"
    );
    assert_eq!(
        session.index().interface_unit_of("std"),
        None,
        "and the index does not know it yet — that is what the slices are for"
    );

    session.index_everything();

    assert_eq!(
        session.index().interface_unit_of("std"),
        Some("gcc/include/c++/bits/std.cc"),
        "after the drain the module is in the index"
    );
    assert!(
        matches!(
            session
                .index()
                .definition("std::string", Path::new("src/main.cpp")),
            Known::Yes(_)
        ),
        "and the names it exports resolve from the importing file"
    );

    assert_eq!(
        session.queue_the_modules_the_open_files_import(),
        0,
        "the second ask finds the module already in the index and queues nothing"
    );

    let _ = DiskFiles;
}

/// **A module interface unit is read as a translation unit, and its closure's facts come from that reading.**
///
/// `import std;` names a module, not a file, so nothing includes `bits/std.cc`; it arrives through
/// `queue_the_modules_a_file_imports`. It used to be queued for a *per-file* cook, whose renderer treats `#include`
/// as a boundary — so every header in the closure kept its **raw** summary, and a raw summary of a header that writes
/// a class head twice under `#if`/`#else` mis-pairs its braces: measured on libstdc++'s `shared_ptr_base.h`,
/// `__shared_ptr_access` was filed as a nested class of `_Sp_counted_ptr_inplace` and every one of its members became
/// unreachable.
///
/// The compiler's reading is the program read: the closure walked, every file rendered **in this unit's environment**,
/// parsed once, and every file's declarations filed from the rendered text. This pins that the session now does that
/// for a module unit — the unit is a program the pump reads, not only a file it cooks.
#[test]
fn a_module_unit_is_read_as_a_translation_unit() {
    use cpp_code_analysis::{OpenDocuments, SessionFiles, WatchFilter};

    let std_cc = "module;\n#include <bits/thing.h>\nexport module std;\nexport namespace std { }\n";
    let thing = "namespace std {\nstruct Widget {\n    void go();\n};\n}\n";
    let demo = "import std;\nint main() { return 0; }\n";
    let files = MemoryFiles::new()
        .with_file("/inc/bits/std.cc", std_cc)
        .with_file("/inc/bits/thing.h", thing)
        .with_file("/p/std_demo.cpp", demo);
    let mut config = CompilerConfig::new();
    config.include_paths = vec![cpp_code_analysis::IncludePath::system("/inc")];
    let mut session = Session::with_config(
        "/p",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("/p"),
        config,
    );
    session.add_project_files([std::path::PathBuf::from("/p/std_demo.cpp")]);
    session.did_open("/p/std_demo.cpp", demo);
    session.index_everything();
    let mut rounds = 0;
    while session.pending() > 0 && rounds < 200 {
        session.advance(16);
        rounds += 1;
    }

    assert_eq!(
        session.read_the_modules_a_file_imports(std::path::Path::new("/p/std_demo.cpp")),
        1,
        "the request path finds the interface unit"
    );
    let mut rounds = 0;
    while session.pending() > 0 && rounds < 400 {
        session.advance(16);
        rounds += 1;
    }

    // The importer's own program first (it is an open document), then the interface unit as a program.
    // **The interface unit is read as a program** — and it comes **first**, because a module is a dependency of the
    // files that import it: until `bits/std.cc` has been read as a translation unit, every header in its closure
    // answers from its raw summary. The importer's own program follows.
    let unit = session
        .read_a_looked_at_unit()
        .expect("the interface unit is a program — that is what this test is about");
    assert!(
        unit.files >= 2,
        "a program read of `bits/std.cc` reads its closure, not one file: {unit:?}"
    );
    let opened = session.read_a_looked_at_unit();
    // **And the importer itself is not read as a program**: its `import std;` is not an `#include`, so a unit read
    // of that file would read it *alone* and replace its rendering with one that has no `std` in it at all.
    let opened = session.read_a_looked_at_unit();
    assert!(
        opened.is_none(),
        "a file that imports a module is served by the module's reading: {opened:?}"
    );
    let header = std::path::Path::new("/inc/bits/thing.h");
    let declarations = session
        .index()
        .cooked_declarations(header)
        .expect("the header has a reading from the unit that included it");
    assert!(
        declarations
            .iter()
            .any(|declaration| &*declaration.name == "Widget"),
        "and `Widget` is one of its declarations: {declarations:?}"
    );
}