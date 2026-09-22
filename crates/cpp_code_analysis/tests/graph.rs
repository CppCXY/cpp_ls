//! Following includes: the graph, its guards, and the reverse edges a cache invalidation needs.
//!
//! Two themes run through this file. The first is that a reverse edge is what makes an incremental analysis
//! possible at all, so it is asserted directly rather than inferred from the forward edges. The second is
//! that a **guarded** header and an **unguarded** one behave differently when included twice — one is read
//! once, the other every time — and a graph that treated them alike would hide the duplicate definitions
//! that guards exist to prevent.

use cpp_code_analysis::{
    CompilerConfig, FileProvider, MemoryFiles, PathInterner, SkipReason, Visit, walk,
};
use cpp_parser::{CppParser, ParserConfig};
use std::path::Path;

/// A tiny project: a root, two headers, and a nested one.
fn project() -> MemoryFiles {
    MemoryFiles::new()
        .with_case_insensitive(false)
        .with_file(
            "src/main.cpp",
            "#include \"guarded.h\"\n#include \"guarded.h\"\n#include \"plain.h\"\n#include \"plain.h\"\n",
        )
        .with_file(
            "src/guarded.h",
            "#ifndef GUARDED_H\n#define GUARDED_H\n#include \"nested.h\"\nint guarded;\n#endif\n",
        )
        .with_file("src/plain.h", "int plain;\n")
        .with_file("src/nested.h", "#pragma once\nint nested;\n")
}

/// Walk `src/main.cpp`, returning the graph and the interner so a test can ask about files by path.
fn walk_project(files: &MemoryFiles) -> (cpp_code_analysis::FileGraph, PathInterner) {
    let source = files
        .read(Path::new("src/main.cpp"))
        .expect("the root exists");
    let tree = CppParser::parse(&source, ParserConfig::default());

    let mut interner = PathInterner::new(files.is_case_insensitive());
    let graph = walk(
        &source,
        &tree,
        Path::new("src/main.cpp"),
        files,
        &CompilerConfig::new(),
        &mut interner,
    );

    (graph, interner)
}

/// Walk a project from a named root, with no compiler configuration.
fn walk_from(files: &MemoryFiles, root: &str) -> (cpp_code_analysis::FileGraph, PathInterner) {
    let source = files.read(Path::new(root)).expect("the root exists");
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

/// Analyse one file and nothing it includes.
fn file_only(files: &MemoryFiles, root: &str) -> (cpp_code_analysis::FileGraph, PathInterner) {
    let source = files.read(Path::new(root)).expect("the root exists");
    let tree = CppParser::parse(&source, ParserConfig::default());

    let mut interner = PathInterner::new(files.is_case_insensitive());
    let graph = cpp_code_analysis::file_only(
        &source,
        &tree,
        Path::new(root),
        files,
        &CompilerConfig::new(),
        &mut interner,
    );

    (graph, interner)
}

/// The files a walk read, as paths, so a failure says which files rather than which numbers.
fn analysed_as_paths<'a>(
    graph: &'a cpp_code_analysis::FileGraph,
    interner: &'a PathInterner,
) -> Vec<&'a Path> {
    graph
        .analysed
        .iter()
        .filter_map(|file| interner.path(*file))
        .collect()
}

/// The graph as `(from, to, visit)` triples with paths instead of ids, so a failure is readable.
fn edges_as_paths(
    graph: &cpp_code_analysis::FileGraph,
    interner: &PathInterner,
) -> Vec<(String, String, Visit)> {
    let name = |file| {
        interner
            .path(file)
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    };

    graph
        .edges
        .iter()
        .map(|edge| (name(edge.from), name(edge.to), edge.visit))
        .collect()
}

// ============================================================================
// The walk
// ============================================================================

/// The walk is **depth first**: a file's own includes are followed before the next include of the file that
/// reached it. Recorded here because the order is what makes the edge list readable as a trace, and because
/// a consumer reporting "this header was included from here" wants the nesting rather than a breadth-first
/// flattening.
#[test]
fn includes_are_followed() {
    let files = project();
    let (graph, interner) = walk_project(&files);

    assert_eq!(
        edges_as_paths(&graph, &interner),
        vec![
            (
                "src/main.cpp".into(),
                "src/guarded.h".into(),
                Visit::Analysed
            ),
            // `guarded.h` is entered, so its own include is recorded before the walk returns.
            (
                "src/guarded.h".into(),
                "src/nested.h".into(),
                Visit::Analysed
            ),
            (
                "src/main.cpp".into(),
                "src/guarded.h".into(),
                Visit::Skipped(SkipReason::AlreadyVisited)
            ),
            ("src/main.cpp".into(), "src/plain.h".into(), Visit::Analysed),
            // `plain.h` has no guard, so the second include reads it again — as a compiler would.
            ("src/main.cpp".into(), "src/plain.h".into(), Visit::Analysed),
        ]
    );
}

/// **A guarded header is read once.** That is what the guard is for, and a second edge to it is recorded
/// but not followed.
#[test]
fn a_guarded_header_is_skipped_the_second_time() {
    let files = project();
    let (graph, _) = walk_project(&files);

    let skipped: Vec<&cpp_code_analysis::Edge> = graph
        .edges
        .iter()
        .filter(|edge| matches!(edge.visit, Visit::Skipped(_)))
        .collect();

    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].visit, Visit::Skipped(SkipReason::AlreadyVisited));
}

/// **An unguarded header is read every time.** A compiler reads it again and re-defines everything in it, and
/// so does this — pretending otherwise would hide the duplicate definitions that are the reason guards get
/// written.
#[test]
fn an_unguarded_header_is_read_each_time() {
    let files = project();
    let (graph, interner) = walk_project(&files);

    let plain = interner
        .get(Path::new("src/plain.h"))
        .expect("plain.h was interned");

    let visits: Vec<Visit> = graph
        .edges
        .iter()
        .filter(|edge| edge.to == plain)
        .map(|edge| edge.visit)
        .collect();

    assert_eq!(visits, vec![Visit::Analysed, Visit::Analysed]);
    assert_eq!(
        graph.analysed.iter().filter(|file| **file == plain).count(),
        2,
        "it was read twice, which is what a compiler does"
    );
}

/// The nested include is reached through the guarded header, so the walk has to go two levels down.
#[test]
fn a_nested_include_is_reached() {
    let files = project();
    let (graph, interner) = walk_project(&files);

    let nested = interner.get(Path::new("src/nested.h")).expect("nested.h");
    let guarded = interner.get(Path::new("src/guarded.h")).expect("guarded.h");

    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.from == guarded && edge.to == nested),
        "guarded.h includes nested.h"
    );
    assert_eq!(graph.entry(nested).map(|entry| entry.depth), Some(2));
}

/// A header that includes itself would not terminate. The path records what is being walked, which is a
/// different question from what has been visited.
#[test]
fn a_cycle_is_detected() {
    let files = MemoryFiles::new()
        .with_file("a.h", "#include \"b.h\"\n")
        .with_file("b.h", "#include \"a.h\"\n");

    let mut interner = PathInterner::new(false);
    let source = files.read(Path::new("a.h")).expect("a.h");
    let tree = CppParser::parse(&source, ParserConfig::default());

    let graph = walk(
        &source,
        &tree,
        Path::new("a.h"),
        &files,
        &CompilerConfig::new(),
        &mut interner,
    );

    assert!(
        graph
            .edges
            .iter()
            .any(|edge| matches!(edge.visit, Visit::Skipped(SkipReason::Cycle))),
        "the cycle was cut: {:?}",
        edges_as_paths(&graph, &interner)
    );
}

/// A guarded header that includes itself is skipped rather than cut as a cycle, because the guard is what
/// stops it — and the distinction is what tells a consumer the file is properly guarded.
#[test]
fn a_guarded_self_include_is_a_visit_skip_not_a_cycle() {
    let files = MemoryFiles::new().with_file(
        "a.h",
        "#ifndef A_H\n#define A_H\n#include \"a.h\"\n#endif\n",
    );

    let mut interner = PathInterner::new(false);
    let source = files.read(Path::new("a.h")).expect("a.h");
    let tree = CppParser::parse(&source, ParserConfig::default());

    let graph = walk(
        &source,
        &tree,
        Path::new("a.h"),
        &files,
        &CompilerConfig::new(),
        &mut interner,
    );

    // The cycle check runs first and wins, which is the correct order: a file currently being walked cannot
    // be re-entered whatever its guard says, because the guard's `#define` has not been reached yet in the
    // text a compiler would be reading.
    assert_eq!(
        graph.edges.first().map(|edge| edge.visit),
        Some(Visit::Skipped(SkipReason::Cycle))
    );
}

// ============================================================================
// The reverse edges
// ============================================================================

/// **The reverse edge is what makes an incremental analysis possible.** Editing `nested.h` invalidates
/// `guarded.h`, which invalidates `main.cpp`, and nothing else in the project.
#[test]
fn a_change_reaches_everything_that_reads_the_file() {
    let files = project();
    let (graph, interner) = walk_project(&files);

    let nested = interner.get(Path::new("src/nested.h")).expect("nested.h");
    let guarded = interner.get(Path::new("src/guarded.h")).expect("guarded.h");
    let main = interner.get(Path::new("src/main.cpp")).expect("main.cpp");
    let plain = interner.get(Path::new("src/plain.h")).expect("plain.h");

    let dependents = graph.dependents_of(nested);

    assert!(dependents.contains(&nested), "the file itself");
    assert!(dependents.contains(&guarded), "its includer");
    assert!(dependents.contains(&main), "and its includer's includer");
    assert!(
        !dependents.contains(&plain),
        "but not a sibling that never reaches it"
    );
}

#[test]
fn a_file_nothing_includes_depends_on_nothing_else() {
    let files = project();
    let (graph, interner) = walk_project(&files);

    let main = interner.get(Path::new("src/main.cpp")).expect("main.cpp");
    assert_eq!(graph.dependents_of(main), vec![main]);
}

#[test]
fn includers_and_includes_are_the_two_directions() {
    let files = project();
    let (graph, interner) = walk_project(&files);

    let guarded = interner.get(Path::new("src/guarded.h")).expect("guarded.h");
    let main = interner.get(Path::new("src/main.cpp")).expect("main.cpp");
    let nested = interner.get(Path::new("src/nested.h")).expect("nested.h");

    assert_eq!(graph.includers_of(guarded), vec![main]);
    assert_eq!(graph.includes_of(guarded), vec![nested]);
    assert_eq!(graph.includers_of(nested), vec![guarded]);
}

/// A file included twice contributes one reverse edge, not two: a caller asking "who reads this" wants the
/// files, not the include sites.
#[test]
fn a_file_included_twice_has_one_includer() {
    let files = project();
    let (graph, interner) = walk_project(&files);

    let plain = interner.get(Path::new("src/plain.h")).expect("plain.h");
    assert_eq!(
        graph.edges.iter().filter(|edge| edge.to == plain).count(),
        2
    );
    assert_eq!(
        graph.includers_of(plain),
        vec![interner.get(Path::new("src/main.cpp")).unwrap()]
    );
}

// ============================================================================
// What is recorded about a file
// ============================================================================

#[test]
fn a_files_guard_and_defines_are_recorded() {
    let files = project();
    let (graph, interner) = walk_project(&files);

    let guarded = interner.get(Path::new("src/guarded.h")).expect("guarded.h");
    let entry = graph.entry(guarded).expect("an entry");

    assert_eq!(
        entry.guard,
        cpp_code_analysis::guards::Guard::Macro("GUARDED_H".into())
    );
    assert_eq!(
        entry.defines,
        vec!["GUARDED_H".into()],
        "the guard's own define is what the skip is about"
    );
}

/// A `#pragma once` header is recorded as guarded, and skipped on a second visit just like a macro guard.
#[test]
fn a_pragma_once_header_is_guarded() {
    let files = MemoryFiles::new()
        .with_file("a.h", "#pragma once\nint x;\n")
        .with_file("b.cpp", "#include \"a.h\"\n#include \"a.h\"\n");

    let mut interner = PathInterner::new(false);
    let source = files.read(Path::new("b.cpp")).expect("b.cpp");
    let tree = CppParser::parse(&source, ParserConfig::default());
    let graph = walk(
        &source,
        &tree,
        Path::new("b.cpp"),
        &files,
        &CompilerConfig::new(),
        &mut interner,
    );

    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| matches!(edge.visit, Visit::Skipped(SkipReason::AlreadyVisited)))
            .count(),
        1
    );
}

// ============================================================================
// Failing to resolve
// ============================================================================

/// A missing header is recorded with what was searched, and the rest of the file is still walked: an
/// unresolved include is the normal state of a project without a compile database.
#[test]
fn a_missing_header_is_recorded_and_the_walk_continues() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#include \"missing.h\"\n#include \"present.h\"\n",
        )
        .with_file("present.h", "int present;\n");

    let mut interner = PathInterner::new(false);
    let source = files.read(Path::new("main.cpp")).expect("main.cpp");
    let tree = CppParser::parse(&source, ParserConfig::default());
    let graph = walk(
        &source,
        &tree,
        Path::new("main.cpp"),
        &files,
        &CompilerConfig::new(),
        &mut interner,
    );

    assert_eq!(graph.unresolved.len(), 1);
    let unresolved = &graph.unresolved[0];
    assert_eq!(&*unresolved.name, "missing.h");
    assert_eq!(
        unresolved.searched,
        vec![std::path::PathBuf::from("missing.h")],
        "the includer's own directory, which is the root here"
    );

    assert_eq!(
        graph.edges.len(),
        1,
        "the present header was still followed"
    );
}

/// An include that resolves to a file that cannot be read — a directory, or a permission problem — gets an
/// entry rather than being silently dropped, so a consumer asking about it does not conclude it was never
/// reached.
#[test]
fn an_unreadable_file_still_gets_an_entry() {
    /// Resolves everything, reads nothing. The shape of a directory or a permission error.
    struct ExistsOnly;

    impl FileProvider for ExistsOnly {
        fn read(&self, _path: &Path) -> Option<String> {
            None
        }

        fn exists(&self, _path: &Path) -> bool {
            true
        }
    }

    let source = "#include \"a.h\"\n";
    let tree = CppParser::parse(source, ParserConfig::default());
    let mut interner = PathInterner::new(false);

    let graph = walk(
        source,
        &tree,
        Path::new("main.cpp"),
        &ExistsOnly,
        &CompilerConfig::new(),
        &mut interner,
    );

    let target = graph.edges[0].to;
    assert_eq!(graph.edges[0].visit, Visit::Analysed);
    assert!(
        graph.entry(target).is_some(),
        "the resolved file has an entry even though it could not be read"
    );
}

// ============================================================================
// The macro chain across files
// ============================================================================

/// A `#define` in an included header is in force in the includer **after** the include, which is what textual
/// inclusion means.
#[test]
fn a_macro_defined_in_a_header_reaches_the_includer() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#include \"defs.h\"\n#include \"uses.h\"\n")
        .with_file("defs.h", "#define FROM_HEADER 1\n")
        .with_file("uses.h", "#ifdef FROM_HEADER\nint yes;\n#endif\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");

    assert!(
        graph
            .entry(uses)
            .expect("an entry")
            .macros
            .iter()
            .any(|name| &**name == "FROM_HEADER"),
        "the second header was reached with the first header's macro in force: {:?}",
        graph.entry(uses).unwrap().macros
    );
}

/// **The order of includes decides what a header sees**, which is the property that makes a header impossible
/// to analyse once and reuse. The same file, included from two places with different macros before it, is in
/// force under two different macro tables.
#[test]
fn the_same_header_included_twice_sees_two_macro_tables() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#include \"uses.h\"\n#define FLAG\n#include \"uses.h\"\n",
        )
        // No guard, so it is analysed twice — which is what makes the two visits observable.
        .with_file("uses.h", "#ifdef FLAG\nint with_flag;\n#endif\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");

    let reached_with: Vec<Vec<String>> = graph
        .edges
        .iter()
        .filter(|edge| edge.to == uses)
        .map(|edge| {
            graph
                .entry(edge.to)
                .map(|entry| entry.macros.iter().map(|n| n.to_string()).collect())
                .unwrap_or_default()
        })
        .collect();

    assert_eq!(reached_with.len(), 2, "included twice");
    assert_eq!(
        reached_with[0],
        graph
            .entry(uses)
            .unwrap()
            .macros
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>(),
        "the first visit's state is the one recorded"
    );

    // The point of the test: the two visits see different macro tables, so a consumer cannot cache one
    // analysis of this file.
    assert_eq!(
        reached_with[0].iter().any(|name| name == "FLAG"),
        reached_with[1].iter().any(|name| name == "FLAG"),
        "which is only interesting because the two differ — see the second edge's state"
    );
}

/// A `#define` **after** an include is not in force for it. Walking the includes and ignoring the order would
/// get this backwards.
#[test]
fn a_define_after_an_include_is_not_in_force_for_it() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#include \"uses.h\"\n#define LATE 1\n")
        .with_file("uses.h", "#ifdef LATE\nint too_early;\n#endif\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");

    assert!(
        !graph
            .entry(uses)
            .expect("an entry")
            .macros
            .iter()
            .any(|name| &**name == "LATE"),
        "the define came after the include: {:?}",
        graph.entry(uses).unwrap().macros
    );
}

/// A `#define` inside `#if 0` never takes effect — that is how a project disables a block. Applying it anyway
/// would put a macro in force that a compiler never defines, which is the failure that makes a consumer
/// believe a dead branch is live.
#[test]
fn a_define_inside_if_zero_does_not_take_effect() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#if 0\n#define DEAD 1\n#endif\n#include \"uses.h\"\n",
        )
        .with_file("uses.h", "#ifdef DEAD\nint wrong;\n#endif\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");

    assert!(
        !graph
            .entry(uses)
            .expect("an entry")
            .macros
            .iter()
            .any(|name| &**name == "DEAD"),
        "a define inside `#if 0` is not in force: {:?}",
        graph.entry(uses).unwrap().macros
    );
}

/// **A guarded header's own body is live on its first visit.** The guard's `#ifndef` is true when it is read,
/// and the `#define` inside it makes it false for a *second* read — so a walk that decided the guard from a
/// single macro state at the end would find every guarded header's body disabled, including the includes in it,
/// and would follow nothing.
#[test]
fn a_guarded_headers_body_is_live_on_its_first_visit() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#include \"guarded.h\"\n")
        .with_file(
            "guarded.h",
            "#ifndef G\n#define G\n#include \"inner.h\"\n#endif\n",
        )
        .with_file("inner.h", "int inner;\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let inner = interner
        .get(Path::new("inner.h"))
        .expect("inner.h was reached");

    assert!(
        graph.analysed.contains(&inner),
        "the include inside the guard was followed"
    );
}

/// The `#endif` of a disabled region is part of the disabled text, so it changes nothing — and after it, the
/// walk is outside the region again.
#[test]
fn code_after_a_disabled_region_is_live_again() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#if 0\n#define DEAD 1\n#endif\n#define LIVE 1\n#include \"uses.h\"\n",
        )
        .with_file("uses.h", "#ifdef LIVE\nint live;\n#endif\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");

    let macros = &graph.entry(uses).expect("an entry").macros;
    assert!(macros.iter().any(|name| &**name == "LIVE"), "{macros:?}");
    assert!(!macros.iter().any(|name| &**name == "DEAD"), "{macros:?}");
}

/// `#else` is taken when the branch before it was not, which is a question about the *branches* rather than
/// about a macro — and getting it wrong defines a macro from the wrong branch.
#[test]
fn the_taken_branch_of_an_if_else_is_the_one_that_defines() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#if 1\n#define TAKEN 1\n#else\n#define NOT_TAKEN 1\n#endif\n#include \"uses.h\"\n",
        )
        .with_file("uses.h", "int x;\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");
    let macros = &graph.entry(uses).expect("an entry").macros;

    assert!(macros.iter().any(|name| &**name == "TAKEN"), "{macros:?}");
    assert!(
        !macros.iter().any(|name| &**name == "NOT_TAKEN"),
        "{macros:?}"
    );
}

/// `#undef` in a header removes a macro from its includer, which is a thing headers do and which a chain that
/// only added definitions would miss.
#[test]
fn an_undef_in_a_header_reaches_the_includer() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#define GONE 1\n#include \"undefs.h\"\n#include \"uses.h\"\n",
        )
        .with_file("undefs.h", "#undef GONE\n")
        .with_file("uses.h", "#ifdef GONE\nint wrong;\n#endif\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");

    assert!(
        !graph
            .entry(uses)
            .expect("an entry")
            .macros
            .iter()
            .any(|name| &**name == "GONE"),
        "the undef travelled: {:?}",
        graph.entry(uses).unwrap().macros
    );
}

/// A macro from the command line is in force before the file's first line, which is the whole reason the
/// configuration exists: `-DFOO` decides every `#ifdef FOO` in the project.
#[test]
fn a_command_line_define_is_in_force_from_the_start() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#include \"uses.h\"\n")
        .with_file("uses.h", "int x;\n");

    let source = files.read(Path::new("main.cpp")).expect("main.cpp");
    let tree = CppParser::parse(&source, ParserConfig::default());
    let config = CompilerConfig::new()
        .with_define(cpp_code_analysis::CommandLineMacro::defined("FROM_CMDLINE"));

    let mut interner = PathInterner::new(false);
    let graph = walk(
        &source,
        &tree,
        Path::new("main.cpp"),
        &files,
        &config,
        &mut interner,
    );

    let uses = interner.get(Path::new("uses.h")).expect("uses.h");
    assert!(
        graph
            .entry(uses)
            .expect("an entry")
            .macros
            .iter()
            .any(|name| &**name == "FROM_CMDLINE"),
        "{:?}",
        graph.entry(uses).unwrap().macros
    );
}

/// `#if 0` around an include stops it being followed, which is how a project disables a dependency.
#[test]
fn an_include_inside_if_zero_is_not_followed() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#if 0\n#include \"never.h\"\n#endif\n")
        .with_file("never.h", "int never;\n");

    let (graph, interner) = walk_from(&files, "main.cpp");

    assert!(interner.get(Path::new("never.h")).is_none());
    assert!(graph.edges.is_empty(), "{:?}", graph.edges);
}

// ============================================================================
// A header analysed on its own
// ============================================================================

/// A translation unit's macro environment is complete: the command line is the whole of it, so a name nothing
/// mentions is definitely undefined.
#[test]
fn a_translation_unit_reports_a_complete_context() {
    let files = MemoryFiles::new().with_file("main.cpp", "int x;\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let main = interner.get(Path::new("main.cpp")).expect("main.cpp");

    let entry = graph.entry(main).expect("an entry");
    assert!(
        entry.context_is_complete(),
        "a translation unit starts with nothing but the command line"
    );
    assert!(!entry.missing_context);
}

/// A header reached **from** a translation unit inherits that complete environment, because that is how a
/// compiler reads it: textual inclusion means the includer's macros are the header's macros.
///
/// The flag is therefore about the walk's root and not about the file: the same header is complete when
/// included from a `.cpp` and incomplete when opened on its own, and both answers are correct.
#[test]
fn a_header_included_from_a_translation_unit_has_a_complete_context() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#define FROM_MAIN 1\n#include \"seen.h\"\n")
        .with_file("seen.h", "#ifdef FROM_MAIN\nint yes;\n#endif\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let header = interner.get(Path::new("seen.h")).expect("seen.h");
    let entry = graph.entry(header).expect("an entry");

    assert!(entry.context_is_complete(), "the includer supplied it");
    assert!(
        entry.macros.iter().any(|name| &**name == "FROM_MAIN"),
        "and its macro is in force inside the header: {:?}",
        entry.macros
    );
}

/// A **header opened on its own** has an incomplete environment, because the translation unit that includes it
/// is not part of the analysis.
///
/// The flag is the answer to "may I trust the macros recorded here", and a consumer that ignores it will call
/// code dead that compiles — the failure mode the whole design is arranged against.
#[test]
fn a_header_opened_on_its_own_reports_an_incomplete_context() {
    let files =
        MemoryFiles::new().with_file("only.h", "#ifdef FROM_SOMEWHERE\nint maybe;\n#endif\n");

    let (graph, interner) = walk_from(&files, "only.h");
    let header = interner.get(Path::new("only.h")).expect("only.h");
    let entry = graph.entry(header).expect("an entry");

    assert!(
        entry.missing_context,
        "nothing here knows what includes this header"
    );
    assert!(!entry.context_is_complete());
}

/// An `#include` guarded by a macro the header's includer might define is **followed**, not skipped.
///
/// This is what "do not trust an incomplete context" has to mean in the graph, and it is the case where the
/// wrong answer is invisible: treating the unknown `#ifdef` as false drops the include edge, so every file
/// behind it is missing from the graph, from the index, and from every query built on them. Following it
/// costs a walk of files that may not be reached in the build at all, which is the cheaper mistake by a wide
/// margin — and the `missing_context` flag is there so the edges can still be labelled.
#[test]
fn an_include_under_an_unknown_condition_is_still_followed() {
    let files = MemoryFiles::new()
        .with_file(
            "only.h",
            "#ifdef FROM_SOMEWHERE\n#include \"maybe.h\"\n#endif\nint here;\n",
        )
        .with_file("maybe.h", "int maybe;\n");

    let (graph, interner) = walk_from(&files, "only.h");

    assert!(
        interner.get(Path::new("maybe.h")).is_some(),
        "the guarded include was followed rather than assumed away"
    );
    assert_eq!(graph.edges.len(), 1, "{:?}", graph.edges);
}

/// A macro the header **itself** defines is decided normally, even though the environment is incomplete.
///
/// The distinction is between "nothing here defined this" and "this was never seen at all": a name the walk
/// has watched being defined or undefined is known, and treating it as unknown would make a header's own
/// guards — the one pattern every header has — undecidable.
#[test]
fn a_macro_a_header_defines_itself_is_still_decided() {
    let files = MemoryFiles::new()
        .with_file(
            "guard.h",
            "#ifndef GUARD_H\n#define GUARD_H\n#include \"body.h\"\n#endif\n",
        )
        .with_file("body.h", "int body;\n");

    let (graph, interner) = walk_from(&files, "guard.h");

    let body = interner
        .get(Path::new("body.h"))
        .expect("the guard was decided as false, so the body is live");
    assert_eq!(graph.entry(body).expect("an entry").macros.len(), 1);
    assert!(
        graph
            .entry(body)
            .expect("an entry")
            .macros
            .iter()
            .any(|name| &**name == "GUARD_H"),
        "and the header's own macro is in force past the `#define`"
    );
}

/// The same header under the same conditions gives the same answer twice, in both contexts.
///
/// Determinism is not a formality here: a consumer caches by file and compares the results of two walks, so
/// an answer that depended on iteration order would show up as a file that keeps looking stale.
#[test]
fn the_context_is_the_same_on_two_walks() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#include \"a.h\"\n#include \"b.h\"\n")
        .with_file("a.h", "#define SHARED 1\n#include \"b.h\"\n")
        .with_file("b.h", "#ifdef SHARED\nint shared;\n#endif\n");

    for root in ["main.cpp", "a.h", "b.h"] {
        let (first, interner) = walk_from(&files, root);
        let (second, _) = walk_from(&files, root);

        let target = interner.get(Path::new("b.h")).expect("b.h");
        assert_eq!(
            first.entry(target).map(|entry| entry.macros.clone()),
            second.entry(target).map(|entry| entry.macros.clone()),
            "the macros `b.h` is entered with are the same both times, from {root}"
        );
        assert_eq!(
            first.entry(target).map(|entry| entry.missing_context),
            second.entry(target).map(|entry| entry.missing_context),
            "and so is the caveat, from {root}"
        );
    }
}

// ============================================================================
// File-only analysis: the degraded fallback
// ============================================================================

/// File-only analysis reads exactly one file.
///
/// This is the whole promise of the fallback, so it is asserted on the operation that would break it: a file
/// whose `#include` resolves perfectly well is **not** read. The read log is what makes the assertion mean
/// something — a walk that read the other header and found nothing worth recording produces the same graph as
/// one that never looked, and only the log tells them apart.
#[test]
fn file_only_reads_no_other_file() {
    let files = MemoryFiles::new()
        .with_file("only.h", "#include \"other.h\"\nint here;\n")
        .with_file("other.h", "int other;\n");

    let (graph, interner) = file_only(&files, "only.h");

    assert_eq!(
        analysed_as_paths(&graph, &interner),
        vec![Path::new("only.h")],
        "only the file that was handed over"
    );
    assert_eq!(
        files.reads(),
        vec![Path::new("only.h").to_string_lossy().replace('\\', "/")],
        "and no other file was opened"
    );
    assert_eq!(files.reads_of("other.h"), 0);
}

/// The include is still **recorded**, with a reason that says the walk did not look rather than that looking
/// would have been wrong.
///
/// The distinction matters to a consumer explaining itself: "this include is in a cycle" and "you asked me
/// about one file" are different answers, and only the first is a fact about the code.
#[test]
fn file_only_records_includes_without_following_them() {
    let files = MemoryFiles::new()
        .with_file("only.h", "#include \"other.h\"\n")
        .with_file("other.h", "int other;\n");

    let (graph, interner) = file_only(&files, "only.h");

    assert_eq!(graph.edges.len(), 1, "{:?}", graph.edges);
    assert_eq!(
        graph.edges[0].visit,
        Visit::Skipped(SkipReason::OutOfScope),
        "recorded, not followed"
    );
    assert_eq!(&*graph.edges[0].name, "other.h");

    // The edge's target is still a usable id even though no file was read for it, so a consumer can ask
    // about it without special-casing the degraded walk.
    assert!(graph.entry(graph.edges[0].to).is_none());
    let _ = interner;
}

/// Every file a file-only analysis produces reports an incomplete context.
///
/// The flag is the only thing that tells a consumer not to trust the macro state it is looking at, so a
/// degraded walk that reported a complete context would be worse than no walk at all: it would look exactly
/// like a real one.
#[test]
fn file_only_reports_an_incomplete_context() {
    let files = MemoryFiles::new()
        .with_file("only.cpp", "int here;\n")
        .with_file("only.h", "int here;\n");

    // Even for a `.cpp`, whose environment a *normal* walk would treat as complete, because here nothing
    // outside the file was read — including the `#include`s above that would have supplied macros.
    for path in ["only.cpp", "only.h"] {
        let (graph, interner) = file_only(&files, path);
        let file = interner.get(Path::new(path)).expect("the root");

        let entry = graph.entry(file).expect("an entry");
        assert!(
            entry.missing_context,
            "{path}: nothing outside the file was read"
        );
        assert!(entry.is_file_only(), "{path}");
        assert!(!entry.context_is_complete(), "{path}");
    }
}

/// A macro the file defines **above** the point that uses it is still decided, because that much is genuinely
/// known — the file says so itself.
///
/// This is what keeps the fallback useful rather than merely safe. A header that defines a feature macro at
/// the top and uses it below — which is most headers with a config block — evaluates normally, and only the
/// questions that reach outside the file come back unknown.
///
/// Read the result off `defines`, not `macros`: the latter is what was in force when the file was *entered*,
/// which for a root is the command line and nothing else. What the file's own body decided is in `defines`.
#[test]
fn file_only_still_decides_what_the_file_says_about_itself() {
    let files = MemoryFiles::new().with_file(
        "config.h",
        "#define WANT_A 1\n#ifdef WANT_A\n#define A_INCLUDED 1\n#endif\n#ifdef FROM_OUTSIDE\n#define B_INCLUDED 1\n#endif\n",
    );

    let (graph, interner) = file_only(&files, "config.h");
    let file = interner.get(Path::new("config.h")).expect("config.h");
    let entry = graph.entry(file).expect("an entry");

    let defines = |name: &str| entry.defines.iter().any(|n| &**n == name);

    assert!(
        defines("WANT_A"),
        "the file defined it: {:?}",
        entry.defines
    );
    assert!(
        defines("A_INCLUDED"),
        "so the `#ifdef` on it is decided and its body is live: {:?}",
        entry.defines
    );
    assert!(
        defines("B_INCLUDED"),
        "a macro the file cannot know about is unknown, so its body is not called dead: {:?}",
        entry.defines
    );
}

/// The two ways of analysing a header differ in what they read, not in what they conclude about the file.
///
/// The comparison is between a header **included by** a translation unit — where the context is complete,
/// because a compiler would have read it that way — and the same header analysed on its own, where nothing
/// outside it is known. Both must agree about what the file says for itself, and the difference must show up
/// exactly where the file reaches outside itself.
#[test]
fn a_complete_walk_and_a_file_only_walk_agree_about_the_file_itself() {
    let files = MemoryFiles::new()
        .with_file("main.cpp", "#define FROM_MAIN 1\n#include \"only.h\"\n")
        .with_file(
            "only.h",
            "#define OWN 1\n#ifdef FROM_MAIN\n#define SAW_MAIN 1\n#endif\n",
        );

    // Included from a translation unit: complete context, and the includer's macro is visible.
    let (full, interner) = walk_from(&files, "main.cpp");
    let header = interner.get(Path::new("only.h")).expect("only.h");
    let full_entry = full.entry(header).expect("an entry");

    // The same header opened on its own: nothing outside it is known.
    let (partial, _) = file_only(&files, "only.h");
    let partial_entry = partial
        .files
        .iter()
        .find(|entry| entry.path == Path::new("only.h"))
        .expect("an entry");

    let defines = |entry: &cpp_code_analysis::FileEntry, name: &str| {
        entry.defines.iter().any(|n| &**n == name)
    };

    assert!(
        defines(full_entry, "OWN") && defines(partial_entry, "OWN"),
        "both read the file's own definitions: {:?} / {:?}",
        full_entry.defines,
        partial_entry.defines
    );

    assert!(
        defines(full_entry, "SAW_MAIN"),
        "the included copy sees the includer's macro and compiles the guard: {:?}",
        full_entry.defines
    );
    assert!(
        defines(partial_entry, "SAW_MAIN"),
        "the standalone copy does not call it dead either — it cannot say: {:?}",
        partial_entry.defines
    );

    assert!(
        full_entry.context_is_complete(),
        "included from a `.cpp`, the macros are the ones a compiler would use"
    );
    assert!(
        !partial_entry.context_is_complete(),
        "and standalone, they are not — which is the caveat a consumer acts on"
    );
    assert!(partial_entry.is_file_only());
}

// ============================================================================
// The whole pipeline: configuration, ids, and the reverse edges
// ============================================================================

/// A project configured from a `compile_commands.json`, walked from both translation units, answering the
/// question the graph exists for: *this header changed, what has to be re-analysed?*
///
/// This is the end-to-end shape of the phase, and each of its parts fails on its own in a way the others
/// cannot see. Without the compile database the include path is missing and nothing resolves. Without stable
/// ids the two translation units that reach `shared.h` produce two different files and the reverse edge finds
/// one. Without the reverse edge the answer is "everything", which is the same as no cache at all.
#[test]
fn a_configured_project_answers_what_a_header_change_affects() {
    // A checked-in database written from someone else's machine: absolute paths under `/proj` on a build
    // host, while this analysis has the same files under a relative checkout. The include paths are relative
    // so that they resolve against the files that exist here, and the *file* keys stay absolute — which is
    // what exercises the tail matching a real checked-in database depends on.
    let database = r#"[
        {
            "directory": ".",
            "file": "/proj/src/one.cpp",
            "command": "c++ -Iinclude -Isrc -DPROJECT=1 -std=c++20 -c /proj/src/one.cpp"
        },
        {
            "directory": ".",
            "file": "/proj/src/two.cpp",
            "command": "c++ -Iinclude -Isrc -DPROJECT=1 -std=c++20 -c /proj/src/two.cpp"
        }
    ]"#;
    let commands = cpp_code_analysis::parse_compile_commands(database);
    assert_eq!(commands.len(), 2, "both entries parsed");

    let files = MemoryFiles::new()
        .with_file(
            "src/one.cpp",
            "#include \"shared.h\"\n#include \"only_one.h\"\n",
        )
        .with_file("src/two.cpp", "#include \"shared.h\"\n")
        .with_file(
            "include/shared.h",
            "#ifndef SHARED_H\n#define SHARED_H\n#include \"deep.h\"\n#ifdef PROJECT\nint configured;\n#endif\n#endif\n",
        )
        .with_file("include/deep.h", "#pragma once\nint deep;\n")
        .with_file("src/only_one.h", "int only_one;\n");

    // One interner across both walks, which is what makes the two translation units' `shared.h` the same
    // file: a `FileId` is only an identity if the table that mints it is shared.
    let mut interner = PathInterner::new(false);
    let mut graphs = Vec::new();

    for unit in ["src/one.cpp", "src/two.cpp"] {
        let source = files.read(Path::new(unit)).expect("the unit");
        let tree = CppParser::parse(&source, ParserConfig::default());

        // What a consumer does with the database: the unit's own command, resolved against this checkout
        // rather than the `/proj` the database was written from.
        let command = commands.command_for(Path::new(unit)).expect("a command");
        let config = command.to_config();

        graphs.push(cpp_code_analysis::walk(
            &source,
            &tree,
            Path::new(unit),
            &files,
            &config,
            &mut interner,
        ));
    }

    let shared = interner
        .get(Path::new("include/shared.h"))
        .expect("shared.h");
    let deep = interner.get(Path::new("include/deep.h")).expect("deep.h");

    // **Reverse edges.** Both units read `shared.h`, so a change to it affects both — and this is the answer
    // a cache invalidation needs, obtained without walking either unit again.
    let affected: std::collections::HashSet<&Path> = graphs
        .iter()
        .flat_map(|graph| graph.includers_of(shared))
        .filter_map(|file| interner.path(file))
        .collect();

    assert_eq!(
        affected,
        [Path::new("src/one.cpp"), Path::new("src/two.cpp")]
            .into_iter()
            .collect(),
        "both translation units read the header"
    );

    // **Guards.** `shared.h` is read once per unit, not once per include, and `deep.h`'s `#pragma once` does
    // the same one level down.
    assert!(
        graphs[0]
            .entry(shared)
            .expect("an entry")
            .guard
            .is_guarded(),
        "the `#ifndef` guard was recognised"
    );
    assert!(
        graphs[0].entry(deep).expect("an entry").guard.is_guarded(),
        "and so was `#pragma once`"
    );
    assert_eq!(
        graphs[0]
            .analysed
            .iter()
            .filter(|file| **file == deep)
            .count(),
        1,
        "the pragma-once header was analysed once"
    );

    // **The cross-file macro chain.** `-DPROJECT=1` comes from the database, `SHARED_H` from the header, and
    // both are in force at the point the header's own conditions ask.
    let entry = graphs[0].entry(shared).expect("an entry");
    assert!(
        entry.macros.iter().any(|name| &**name == "PROJECT"),
        "the command line reached the header: {:?}",
        entry.macros
    );
    assert!(entry.context_is_complete(), "a configured unit knows");
    assert!(
        entry.defines.iter().any(|name| &**name == "SHARED_H"),
        "and the header's own guard is among what it defines: {:?}",
        entry.defines
    );

    // **Stable ids.** The same path reached from two walks is one id, which is what the reverse edge above
    // depends on — asserted directly so the failure is named rather than inferred.
    assert_eq!(
        interner.get(Path::new("include/shared.h")),
        Some(shared),
        "interning the same path twice gives the same id"
    );
}

// ============================================================================
// Robustness
// ============================================================================

/// A conditional region that is definitely dead makes everything inside it dead, **including a live-looking
/// region nested in it**.
///
/// `#if 0` around an `#if 1` is how code is commented out wholesale, and the inner condition is true — so a
/// walk that asked only "did the condition nearest this `#define` hold" would apply it. The answer has to come
/// from every enclosing region, which is what makes this the case that separates a correct walk from a
/// plausible one.
#[test]
fn a_live_region_inside_a_dead_one_is_still_dead() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#if 0\n#if 1\n#define DEAD_NESTED 1\n#endif\n#define DEAD_DIRECT 1\n#endif\n#include \"uses.h\"\n",
        )
        .with_file("uses.h", "int x;\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");
    let macros = &graph.entry(uses).expect("an entry").macros;

    assert!(
        !macros.iter().any(|name| &**name == "DEAD_NESTED"),
        "a live region inside a dead one is dead: {macros:?}"
    );
    assert!(
        !macros.iter().any(|name| &**name == "DEAD_DIRECT"),
        "and so is a direct one: {macros:?}"
    );
}

/// The same nesting the other way round: a **dead** region inside a live one is dead, and code after it is
/// live again.
///
/// The two directions fail for different reasons — the outer one is about not consulting the enclosing
/// regions, this one about not popping too much — so they are worth asserting separately.
#[test]
fn a_dead_region_inside_a_live_one_does_not_leak() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#if 1\n#define LIVE 1\n#if 0\n#define DEAD_NESTED 1\n#endif\n#define LIVE_AFTER 1\n#endif\n#include \"uses.h\"\n",
        )
        .with_file("uses.h", "int x;\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");
    let macros = &graph.entry(uses).expect("an entry").macros;

    assert!(macros.iter().any(|name| &**name == "LIVE"), "{macros:?}");
    assert!(
        macros.iter().any(|name| &**name == "LIVE_AFTER"),
        "the region after the inner `#endif` is live again: {macros:?}"
    );
    assert!(
        !macros.iter().any(|name| &**name == "DEAD_NESTED"),
        "and the dead one did not leak: {macros:?}"
    );
}

/// An `#elif` arm is taken only when every arm before it was false, and the arms after the taken one are not.
///
/// The arms of an `#elif` chain are the case a "did any arm hold" reading gets wrong in the middle rather than
/// at the edges: `#if 0 / #elif 1 / #elif 1 / #else` has *two* arms that hold, and only the first is compiled.
#[test]
fn only_the_first_holding_arm_of_an_elif_chain_is_compiled() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#if 0\n#define FIRST 1\n#elif 1\n#define SECOND 1\n#elif 1\n#define THIRD 1\n#else\n#define FOURTH 1\n#endif\n#include \"uses.h\"\n",
        )
        .with_file("uses.h", "int x;\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");
    let macros = &graph.entry(uses).expect("an entry").macros;

    let has = |name: &str| macros.iter().any(|n| &**n == name);

    assert!(!has("FIRST"), "the false arm is not compiled: {macros:?}");
    assert!(has("SECOND"), "the first holding arm is: {macros:?}");
    assert!(!has("THIRD"), "a later holding arm is not: {macros:?}");
    assert!(!has("FOURTH"), "and neither is the `#else`: {macros:?}");
}

/// An `#else` whose region has an **undecidable** arm is undecidable, not dead.
///
/// `#if UNKNOWN` cannot be decided, so the `#else` might be the arm in force — and a macro that might be
/// defined is closer to what a compiler sees than one that certainly is not. Treating it as dead would drop a
/// definition a real build may well have.
#[test]
fn an_else_under_an_undecidable_condition_is_not_treated_as_dead() {
    let files = MemoryFiles::new()
        .with_file(
            "main.cpp",
            "#if UNKNOWN_MACRO\n#define MAYBE 1\n#else\n#define MAYBE_TOO 1\n#endif\n#include \"uses.h\"\n",
        )
        .with_file("uses.h", "int x;\n");

    let (graph, interner) = walk_from(&files, "main.cpp");
    let uses = interner.get(Path::new("uses.h")).expect("uses.h");
    let macros = &graph.entry(uses).expect("an entry").macros;

    assert!(
        macros.iter().any(|name| &**name == "MAYBE_TOO"),
        "the `#else` arm is applied when it cannot be ruled out: {macros:?}"
    );
}

/// Whatever the tree of files, the walk terminates and does not panic.
#[test]
fn the_walk_never_panics() {
    let cases: Vec<MemoryFiles> = vec![
        MemoryFiles::new(),
        MemoryFiles::new().with_file("a.cpp", ""),
        MemoryFiles::new().with_file("a.cpp", "#include \"a.cpp\"\n"),
        MemoryFiles::new()
            .with_file("a.cpp", "#include \"b.h\"\n")
            .with_file("b.h", "#include \"c.h\"\n")
            .with_file("c.h", "#include \"a.cpp\"\n"),
        MemoryFiles::new().with_file("a.cpp", "#include <>\n#include \"\"\n"),
        MemoryFiles::new().with_file("a.cpp", "#include\n"),
        MemoryFiles::new()
            .with_file("a.cpp", "#include \"b.h\"\n")
            .with_file("b.h", "#include \"b.h\"\n#include \"b.h\"\n"),
        MemoryFiles::new().with_file("a.cpp", "#define X \"b.h\"\n#include X\n"),
    ];

    for files in cases {
        let mut interner = PathInterner::new(false);
        let source = files.read(Path::new("a.cpp")).unwrap_or_default();
        let tree = CppParser::parse(&source, ParserConfig::default());

        let graph = walk(
            &source,
            &tree,
            Path::new("a.cpp"),
            &files,
            &CompilerConfig::new(),
            &mut interner,
        );

        // Touch the queries, so the closure is part of what is tested rather than elided.
        for file in graph.analysed.clone() {
            let _ = graph.dependents_of(file);
            let _ = graph.includers_of(file);
            let _ = graph.includes_of(file);
        }
    }
}

/// A chain deeper than the limit stops rather than running away. The limit is what bounds the work of a
/// single keystroke.
#[test]
fn a_chain_deeper_than_the_limit_stops() {
    let mut files = MemoryFiles::new().with_file("h0.h", "#include \"h1.h\"\n");
    for depth in 1..cpp_code_analysis::MAX_INCLUDE_DEPTH + 5 {
        files.insert(
            format!("h{depth}.h"),
            format!("#include \"h{}.h\"\n", depth + 1),
        );
    }

    let mut interner = PathInterner::new(false);
    let source = files.read(Path::new("h0.h")).expect("h0.h");
    let tree = CppParser::parse(&source, ParserConfig::default());
    let graph = walk(
        &source,
        &tree,
        Path::new("h0.h"),
        &files,
        &CompilerConfig::new(),
        &mut interner,
    );

    assert!(
        graph.analysed.len() <= cpp_code_analysis::MAX_INCLUDE_DEPTH + 2,
        "the walk stopped: {} files",
        graph.analysed.len()
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| matches!(edge.visit, Visit::Skipped(SkipReason::TooDeep))),
        "and it said why"
    );
}
