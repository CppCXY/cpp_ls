//! **One walk of a translation unit, read from by every file in it.**
//!
//! The per-file reading — "walk *this* file's include closure and hand it the result" — costs one full walk per
//! file, and the census put a number on it: 255 files of the Windows SDK corpus, 2 489 142 macro entries
//! materialised, 17 618 794 conditional facts evaluated, 147 s of a 174 s run. `TranslationUnit` is the
//! timeline that replaces it: the unit is walked **once**, and a file's environment is read out of that walk.
//!
//! # What these tests are for
//!
//! A cheaper walk is worthless if it answers differently, and "differently" here is not a diagnostic — it is a
//! macro that is in force in one reading and not in the other, which shows up much later as a parse that reads
//! something else. So the central test compares the two readings **question by question**: for every name and
//! every offset that appears in the file, `kind_of`, `body_text_of` and `parameters_of` must agree with what the
//! per-file closure walk built.
//!
//! The two readings *should* differ in exactly one place, and it is an improvement rather than a regression: the
//! inherited state of a header. The per-file walk could only approximate it from one includer (the probe's
//! BFS-chosen one), while the timeline has the real translation unit state at the `#include`. The test asserts the
//! agreement on a file whose closure says everything the prefix would, so the approximation is not in play; the
//! census is where the difference is measured.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::graph::Marked;
use cpp_code_analysis::{
    CompilerConfig, FileIndexer, FileSummary, MacroDefinitions, MemoryFiles, SummaryKey,
    TranslationUnit, TranslationUnitCache,
};

/// A file set in memory, indexed once, with the two ways of asking for a file's environment.
struct Unit {
    summaries: HashMap<PathBuf, FileSummary>,
    sources: HashMap<PathBuf, String>,
    /// The provider the set came from — kept because the cache validates against **files**, not against a
    /// summary map, and a test that hands it one must hand it the same texts the walk read.
    files: MemoryFiles,
    seed: Marked,
}

impl Unit {
    fn new(files: &[(&str, &str)]) -> Self {
        let mut provider = MemoryFiles::new();
        for (path, text) in files {
            provider.insert(path, *text);
        }

        // Every directory of the set is on the search path, so `#include "x.h"` resolves the way the probe's
        // corpus does — the arrangement is the caller's, and this test only needs one that resolves.
        let mut config = CompilerConfig::default();
        for (path, _) in files {
            if let Some(parent) = Path::new(path).parent() {
                config = config.with_include_path(parent.to_path_buf());
            }
        }

        let indexer = FileIndexer::new(&provider, &config);
        let key = SummaryKey::new(0, 0);
        let mut summaries = HashMap::new();
        let mut sources = HashMap::new();
        for (path, text) in files {
            let path = PathBuf::from(path);
            summaries.insert(path.clone(), indexer.index(&path, text, key));
            sources.insert(path, (*text).to_string());
        }

        Unit {
            summaries,
            sources,
            files: provider,
            // A seed with the two names every corpus asks about, so a `#ifdef` in the fixtures is answerable.
            seed: {
                let mut marked = Marked::default();
                marked.define_on_the_command_line("__cplusplus", Some("202002L"));
                marked.define_on_the_command_line("_WIN32", Some("1"));
                marked
            },
        }
    }

    fn look_up(&self, wanted: &Path) -> Option<(&FileSummary, &str)> {
        Some((
            self.summaries.get(wanted)?,
            self.sources.get(wanted).map(String::as_str).unwrap_or(""),
        ))
    }

    /// The per-file reading: walk this file's own include closure.
    fn per_file(&self, path: &str, definitions: &mut MacroDefinitions) -> cpp_parser::MacroEnvironment {
        let path = PathBuf::from(path);
        let summary = self.summaries.get(&path).expect("indexed");
        let evidence = cpp_code_analysis::macros_from_the_closure_with_bodies(
            summary,
            |wanted| self.look_up(wanted),
            &self.seed,
            definitions,
        );

        cpp_parser::MacroEnvironment::from_included_macros(evidence.macros)
            .with_bodies_in_force(evidence.conditional_bodies)
    }

    /// The one-walk reading: the unit's timeline, and the file's view of it.
    fn timeline_of(
        &self,
        root: &str,
        definitions: &mut MacroDefinitions,
    ) -> TranslationUnit {
        let root = PathBuf::from(root);
        TranslationUnit::walk(
            self.summaries.get(&root).expect("indexed"),
            |wanted| self.look_up(wanted),
            &self.seed,
            definitions,
        )
    }
}

/// Every question the parser can ask of an environment, asked of both readings.
fn answers(
    environment: &cpp_parser::MacroEnvironment,
    name: &str,
    offset: usize,
) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let kind = environment.kind_of(name, offset).map(|kind| format!("{kind:?}"));
    (
        kind,
        environment.body_text_of(name, offset).map(str::to_string),
        environment.parameters_of(name, offset).map(str::to_string),
        environment.body_text_in_force(name).map(str::to_string),
    )
}

/// **The one-walk reading answers exactly what the per-file walk answered.**
///
/// The fixture is the shape the corpus is made of: a header that defines macros **conditionally**, a header that
/// includes it and defines more, a source file that includes both, and a header that names something neither of
/// them defines.
#[test]
fn the_timeline_answers_what_the_per_file_walk_answered() {
    let files = [
        (
            "/p/config.h",
            "#ifndef _CONFIG_H_\n\
             #define _CONFIG_H_\n\
             #ifdef __cplusplus\n\
             #define SA(id) id\n\
             #else\n\
             #define SA(id) SA_##id\n\
             #endif\n\
             #define REPEATABLE [repeatable]\n\
             #endif\n",
        ),
        (
            "/p/api.h",
            "#include \"config.h\"\n\
             #define API_CALL __cdecl\n\
             #if _WIN32\n\
             #define WINDOWS 1\n\
             #endif\n\
             API_CALL void f(SA(int) value);\n",
        ),
        (
            "/p/main.cpp",
            "#include \"api.h\"\n\
             int before = API_CALL;\n\
             WINDOWS\n",
        ),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let per_file = unit.per_file("/p/api.h", &mut definitions);
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);
    let view = timeline
        .environment_of(Path::new("/p/api.h"))
        .expect("the unit reaches api.h");

    // Every name the unit mentions, at every offset the file has — the question set is the file's own length, so
    // a difference cannot hide in an offset the test forgot to ask about.
    let source = unit.sources.get(Path::new("/p/api.h")).expect("read");
    let names = ["SA", "REPEATABLE", "API_CALL", "WINDOWS", "_CONFIG_H_", "f", "value"];
    for name in names {
        for offset in (0..source.len()).step_by(7) {
            assert_eq!(
                answers(&view, name, offset),
                answers(&per_file, name, offset),
                "`{name}` at offset {offset} of api.h: the timeline and the per-file walk disagree"
            );
        }
    }

    // …and the precondition of the whole comparison, asserted rather than assumed: both readings really do know
    // the names that come **from the include**, so "they agree" is not "they are both empty".
    assert_eq!(
        per_file.body_text_of("API_CALL", source.len()),
        None,
        "the per-file walk carries what the includes contribute, and `API_CALL` is api.h's own `#define`"
    );
    assert_eq!(
        view.body_text_in_force("SA").map(str::trim),
        Some("id"),
        "`#ifdef __cplusplus` was answered as C++ in the walk, so the C++ spelling of `SA` is the one in force — \
         and a definition inside a taken branch is a **body**, not a definition, in both readings"
    );
    assert_eq!(
        per_file.body_text_in_force("SA").map(str::trim),
        Some("id"),
        "the per-file walk answered the same branch"
    );
    assert_eq!(
        view.body_text_of("API_CALL", source.len()),
        None,
        "a file's own facts are the parser's own reading in both readings — see `environment_at`'s contract"
    );
    // The record still *has* the fact, with the file and the offset it was written at: the exclusion is the
    // environment's, not the timeline's. (`range` is the **name**, deliberately — see `MacroFact` — so the offset
    // lands on `API_CALL` and not on the `#` that introduces it.)
    assert!(
        timeline
            .facts_of(Path::new("/p/api.h"))
            .any(|(name, at)| name == "API_CALL" && source[at..].starts_with("API_CALL")),
        "the timeline records what the file wrote, at the offset it wrote it"
    );
}

/// **A file's environment carries the state of the unit it is read in** — the difference from the per-file walk,
/// and the reason the timeline is not merely cheaper.
///
/// The old reading gave a header a context built from **one includer**, found by a breadth-first walk of the
/// corpus, and the doc records what that cost: six headers looked *broken* by a switch when each read cleanly on
/// its own — the "context" was a property of the file the probe picked, not of the header. The timeline has the
/// real thing: the state of the translation unit at the `#include`.
///
/// So this test asserts a difference on purpose, in both directions, on one fixture.
#[test]
fn a_files_environment_carries_the_state_of_the_unit_it_is_read_in() {
    let files = [
        ("/p/api.h", "int inside = FROM_THE_SOURCE;\n"),
        (
            "/p/main.cpp",
            "#define FROM_THE_SOURCE 1\n#include \"api.h\"\n",
        ),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let per_file = unit.per_file("/p/api.h", &mut definitions);
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);
    let view = timeline
        .environment_of(Path::new("/p/api.h"))
        .expect("the unit reaches api.h");

    assert_eq!(
        per_file.kind_of("FROM_THE_SOURCE", 0),
        None,
        "api.h's own closure says nothing about it — the name is the source file's"
    );
    assert!(
        view.kind_of("FROM_THE_SOURCE", 0).is_some(),
        "…and the unit that includes api.h has it in force, which is what the header really sees"
    );
    assert_eq!(
        view.body_text_of("FROM_THE_SOURCE", 0).map(str::trim),
        Some("1"),
        "with the body the compilation had"
    );
}

/// **A definition is in force from the `#include` that brought it in**, and the timeline's offsets say so.
///
/// This is the property that makes the environment *positional*, and the one a careless timeline loses: if every
/// inherited definition is simply put at offset 0 — or worse, if the file's own offsets are used for a file that
/// wrote them somewhere else — a name becomes a macro before the line that defines it, and rules that read shape
/// because no table knows the name switch off for the whole file.
#[test]
fn an_included_definition_is_in_force_from_the_include() {
    let files = [
        ("/p/late.h", "#define LATE 1\n"),
        (
            "/p/user.h",
            "int before = LATE;\n#include \"late.h\"\nint after = LATE;\n",
        ),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/user.h", &mut definitions);
    let view = timeline.root_environment();

    let source = unit.sources.get(Path::new("/p/user.h")).expect("read");
    let include_end = source.find("#include \"late.h\"").expect("the include") + "#include \"late.h\"".len();

    assert_eq!(
        view.kind_of("LATE", include_end - 2),
        None,
        "before the include, nothing here has defined `LATE`"
    );
    assert!(
        view.kind_of("LATE", include_end + 2).is_some(),
        "after the include, it is a macro — from the end of the `#include`, not from offset 0"
    );
}

/// **A file the unit never reaches has no environment**, and that is an answer rather than a failure.
///
/// The per-file reading gives every file a closure of its own; the unit reading deliberately does not, because
/// "what does this file see" has no answer outside a translation unit that includes it. `None` is what says so,
/// and the caller that wants a context for such a file has to name the unit that gives it one.
#[test]
fn a_file_outside_the_unit_has_no_environment() {
    let files = [
        ("/p/api.h", "#define API 1\n"),
        ("/p/main.cpp", "#include \"api.h\"\n"),
        ("/p/unrelated.h", "#define UNRELATED 1\n"),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);

    assert!(timeline.environment_of(Path::new("/p/api.h")).is_some());
    assert!(
        timeline.environment_of(Path::new("/p/unrelated.h")).is_none(),
        "nothing includes it, so nothing here says what it sees"
    );
    assert_eq!(
        timeline.files().count(),
        2,
        "the walk entered the source and the header it includes, and nothing else"
    );
}

/// **One walk, not one per file** — the property the whole type exists for, asserted on the read log.
///
/// A count of reads is the only way to see this: two readings that reach the same environments produce the same
/// *answers*, and the difference is entirely in how many times the files were walked. `MemoryFiles` counts reads
/// for exactly this question (see its own note), so the test asks it directly.
#[test]
fn the_unit_walks_its_files_once() {
    let files = [
        ("/p/a.h", "#define A 1\n"),
        ("/p/b.h", "#include \"a.h\"\n#define B 1\n"),
        ("/p/c.h", "#include \"a.h\"\n#include \"b.h\"\n#define C 1\n"),
        ("/p/main.cpp", "#include \"c.h\"\n"),
    ];

    let mut provider = MemoryFiles::new();
    for (path, text) in files {
        provider.insert(path, text);
    }
    let mut config = CompilerConfig::default();
    config = config.with_include_path(PathBuf::from("/p"));
    let indexer = FileIndexer::new(&provider, &config);
    let key = SummaryKey::new(0, 0);

    let mut summaries = HashMap::new();
    let mut sources = HashMap::new();
    for (path, text) in files {
        let path = PathBuf::from(path);
        summaries.insert(path.clone(), indexer.index(&path, text, key));
        sources.insert(path, (*text).to_string());
    }

    let mut seed = Marked::default();
    seed.define_on_the_command_line("__cplusplus", Some("202002L"));
    let look_up = |wanted: &Path| {
        Some((
            summaries.get(wanted)?,
            sources.get(wanted).map(String::as_str).unwrap_or(""),
        ))
    };
    let mut definitions = MacroDefinitions::default();
    let timeline = TranslationUnit::walk(
        summaries.get(Path::new("/p/main.cpp")).expect("indexed"),
        look_up,
        &seed,
        &mut definitions,
    );

    // `a.h` is included twice (`c.h` and `b.h` both take it) and walked once: the guard idiom means a second read
    // defines nothing new, which is the rule `walk_one_file` states. Every file is read at most once for the whole
    // unit — where the per-file reading read the closure of each file, so `a.h` was read once per consumer.
    for path in ["/p/a.h", "/p/b.h", "/p/c.h", "/p/main.cpp"] {
        assert!(
            provider.reads_of(path) <= 1,
            "{path} was read {} times for one unit",
            provider.reads_of(path)
        );
    }
    assert_eq!(timeline.files().count(), 4, "four files, one walk");
}

/// **A unit can be kept and read back** — the cache layer's acceptance, on a fixture rather than a corpus.
///
/// Three properties, and the second is the one that makes it usable rather than merely fast:
///
/// 1. an entry written by one run is served to the next;
/// 2. what is served **answers exactly like what was walked** — a cache of macro state that decodes into a
///    slightly different timeline produces a *reading*, not a diagnostic, and every rule downstream would then
///    answer about a header nobody has;
/// 3. a file in the closure whose contents moved **refuses** the entry, which is the whole validity key.
#[test]
fn a_walked_unit_can_be_kept_and_read_back() {
    let files = [
        ("/p/config.h", "#ifndef _CONFIG_H_\n#define _CONFIG_H_\n#define SA(id) id\n#endif\n"),
        ("/p/api.h", "#include \"config.h\"\n#define API_CALL __cdecl\nAPI_CALL void f(SA(int) value);\n"),
        ("/p/main.cpp", "#include \"api.h\"\nint before = API_CALL;\n"),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let walked = unit.timeline_of("/p/main.cpp", &mut definitions);

    let directory = std::env::temp_dir().join(format!(
        "cppls-tu-kept-{}-{}",
        std::process::id(),
        cpp_code_analysis::fnv1a64(b"a_walked_unit_can_be_kept_and_read_back")
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let cache = TranslationUnitCache::new(&directory);

    let root = Path::new("/p/main.cpp");
    cache
        .put(root, 11, &walked, &unit.files)
        .expect("the entry writes");
    let served = cache
        .get(root, 11, &unit.files)
        .expect("the entry is served while the closure is unchanged");

    assert_eq!(
        served.len(),
        walked.len(),
        "every fact came back, so the views below are answering about the same timeline"
    );
    assert_eq!(
        served.files().count(),
        walked.files().count(),
        "…and every frame"
    );
    assert_eq!(
        (served.conditional_facts, served.facts_in_force),
        (walked.conditional_facts, walked.facts_in_force),
        "the two counters the census prints came back too"
    );

    // The answers, question by question, on the file whose environment is the point of the exercise.
    let source = unit.sources.get(Path::new("/p/api.h")).expect("read");
    let from_disk = served
        .environment_of(Path::new("/p/api.h"))
        .expect("the served unit reaches api.h");
    let from_the_walk = walked
        .environment_of(Path::new("/p/api.h"))
        .expect("the walked unit reaches api.h");
    for name in ["SA", "API_CALL", "_CONFIG_H_", "f", "value"] {
        for offset in (0..source.len()).step_by(5) {
            assert_eq!(
                answers(&from_disk, name, offset),
                answers(&from_the_walk, name, offset),
                "`{name}` at offset {offset}: the decoded unit answers differently from the walked one"
            );
        }
    }

    // **A header in the closure changes** — the entry is refused, not served with the old macros.
    let mut changed = MemoryFiles::new();
    for (path, text) in &files {
        let text = if *path == "/p/config.h" {
            "#ifndef _CONFIG_H_\n#define _CONFIG_H_\n#define SA(id) SA_##id\n#endif\n"
        } else {
            *text
        };
        changed.insert(path, text);
    }
    assert!(
        cache.get(root, 11, &changed).is_none(),
        "an edited header in the closure refuses the entry"
    );

    let _ = std::fs::remove_dir_all(&directory);
}