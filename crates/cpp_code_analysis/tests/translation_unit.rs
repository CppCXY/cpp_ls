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
    CompilerConfig, FileIndexer, FileSummary, MacroBindings, MacroDefinitions, MemoryFiles,
    SummaryKey, TranslationUnit, TranslationUnitCache,
};
use cpp_parser::MacroFacts;

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
///
/// `&dyn MacroFacts` rather than either of the two types, because comparing the readings is the whole point: the
/// per-file walk answers them out of a materialised `MacroEnvironment` and the unit answers them out of a
/// [`cpp_code_analysis::MacroView`], and a test that had to name one of them could not ask both the same question.
fn answers(
    environment: &dyn cpp_parser::MacroFacts,
    name: &str,
    offset: usize,
) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let kind = environment.kind_of(name, offset).map(|kind| format!("{kind:?}"));
    (
        kind,
        environment.body_text_of(name, offset).map(str::to_string),
        environment.parameters_of(name, offset).map(|list| list.to_string()),
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

/// **The unit cooks as one program**, not as a pile of headers.
///
/// Cooking one file at a time answers "does this header read on its own" — it cannot answer "does the program
/// read", because a declaration in one file and a use in another are two streams with nothing tying them
/// together. `cook_the_unit` is that stream: the walk's own order (which is include order) with each include
/// spliced in where the `#include` ended, and a map that says which file every token stands in.
#[test]
fn the_unit_cooks_as_one_stream_in_include_order() {
    let files = [
        ("/p/config.h", "#define WIDTH 4\nstruct Cfg { int a; };\n"),
        (
            "/p/api.h",
            "#include \"config.h\"\n#define API WIDTH\nstruct Api { Cfg c; };\n",
        ),
        ("/p/main.cpp", "#include \"api.h\"\nint main_use = API;\n"),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);
    let shared = timeline.definitions();

    let stitched = timeline.cook_the_unit(&unit.sources, &shared, None, true);
    assert_eq!(stitched.missing, 0, "every file the unit reached has text here");
    assert_eq!(
        stitched.files_with_tokens(),
        3,
        "all three files contributed a token: {}",
        stitched.text
    );

    // The order is the walk's: what a file includes comes before its own text, and a header's own include comes
    // before the header's.
    let at = |needle: &str| {
        stitched
            .text
            .find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is in the stream: {}", stitched.text))
    };
    assert!(at("struct Cfg") < at("struct Api"), "{}", stitched.text);
    assert!(at("struct Api") < at("main_use"), "{}", stitched.text);

    // **It is a program**: `Cfg` is a type the stream declared one file earlier, `API` came from a header and
    // expanded to a value `config.h` defines, and the whole thing parses.
    let tree = cpp_parser::CppParser::parse(&stitched.text, cpp_parser::ParserConfig::default());
    assert!(
        tree.get_errors().is_empty(),
        "the stitched unit has errors: {:?} in {}",
        tree.get_errors(),
        stitched.text
    );
    assert!(
        stitched.text.contains("= 4 ;"),
        "`API` expanded to `WIDTH`'s value, which is defined two files away: {}",
        stitched.text
    );

    // **The map says which file a token stands in**, and where in that file to act: `Api` is api.h's text…
    let api = Path::new("/p/api.h");
    let api_text = unit.sources.get(api).expect("read");
    let api_at = stitched.text.find("Api").expect("in the stream");
    let (file, written) = stitched.written_at(api_at).expect("a token is there");
    assert_eq!(stitched.file_of(file), Some(api));
    assert_eq!(&api_text[written.start_offset..written.end_offset()], "Api");

    // …and the `4`, which was written in `config.h` and pasted by a macro `api.h` defines, **stands in
    // main.cpp**: that is where the reader can act on it, and the map points at the call site.
    let main = Path::new("/p/main.cpp");
    let main_text = unit.sources.get(main).expect("read");
    let four = stitched.text.find(" 4 ").expect("the expansion is in the stream");
    let (file, written) = stitched.written_at(four).expect("a token is there");
    assert_eq!(
        stitched.file_of(file),
        Some(main),
        "a macro's token stands in the file that invoked it"
    );
    assert_eq!(
        written.start_offset,
        main_text.find("API").expect("the invocation"),
        "and it points at the call site, not at a body in another file"
    );
}

/// **A declaration that begins with a token out of another file's macro body is still that file's declaration.**
///
/// The contract [`RenderedUnit::written_span`] needs: **the file an answer names and the range it gives are two
/// facts about one text**, so the range has to end inside it.
///
/// A `UnitSpan` carries two different things, and answering with the wrong one would be silent:
///
/// ```text
/// span.file     which file's cook produced the token — the file it **stands in**, and the only thing that
///               can say which file a declaration is in
/// span.written  where to act on it — the token's own range, or the call site for one a macro produced
/// ```
///
/// The shape that separates them is a declaration whose **whole text came out of another file's macro body**: the
/// invocation `DECLARE_WIDGET` is what `decl.h` wrote, and the `struct Widget { … }` in the stream is `macros.h`'s
/// text pasted in. Read the file out of the *hint* and the answer is a position in `macros.h`'s body while the
/// declaration stands in `decl.h`.
///
/// **It is not a live bug, and this test is why that is a measurement rather than a belief.**
/// `ExpandedToken::diagnostic_range` already answers with the **outermost call site** — a position in the invoking
/// file — so the hint and the standing file agree, and the assertion below holds on this crate's implementation.
/// Nothing *enforced* it before: the failure this class of answer produced elsewhere was a 93 971-byte class body
/// filed under a 1.2 KB header with every other field of the fact correct. So the test pins the invariant rather
/// than a past mistake, and [`RenderedUnit::span_lands_in`] is the check that turns a future violation into a
/// counted `unplaced` instead of a wrong answer.
#[test]
fn a_declaration_starts_in_the_file_it_stands_in() {
    let files = [
        (
            "/p/macros.h",
            "#define DECLARE_WIDGET struct Widget { int size; };\n",
        ),
        (
            "/p/decl.h",
            "#include \"macros.h\"\nnamespace outer {\nDECLARE_WIDGET\n}\n",
        ),
        ("/p/main.cpp", "#include \"decl.h\"\nint use = 0;\n"),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);
    let shared = timeline.definitions();
    let stream = timeline.cook_the_unit(&unit.sources, &shared, None, true);

    // The stream has the expansion, not the invocation: `decl.h` wrote a name, the stream has a struct.
    assert!(
        stream.text.contains("struct Widget { int size ; } ;"),
        "the macro expanded into the stream: {}",
        stream.text
    );

    let tree = cpp_parser::CppParser::parse(&stream.text, cpp_parser::ParserConfig::default());
    let widget = tree
        .get_red_root()
        .descendants()
        .find(|node| cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::StructDef)
        .expect("the struct is in the stream");
    let range = cpp_parser::source_range(widget.text_range());

    let (file, written) = stream.written_span(range).expect("it maps");
    assert_eq!(
        stream.file_of(file),
        Some(Path::new("/p/decl.h")),
        "the declaration **stands in** decl.h, which is the file whose invocation produced it"
    );

    // …and the answer lands in that file — the invariant a hint from another file would break. Asked as a bounds
    // check rather than through `span_lands_in` so that the test puts the question to the mapping itself, which is
    // the layer a consumer reads; `span_lands_in` is what the index asks on every fact.
    let decl = &unit.sources[Path::new("/p/decl.h")];
    let acted_on = decl
        .get(written.start_offset..written.end_offset())
        .unwrap_or_else(|| {
            panic!(
                "the answer does not land in the file it names: {written:?} of {} bytes",
                decl.len()
            )
        });
    assert!(
        acted_on.contains("DECLARE_WIDGET"),
        "the range is the declaration as that file spells it — the invocation, not the body: {acted_on:?}"
    );
}

/// **Taking a file out of the stream keeps every offset where it was.**
///
/// The quarantine path blanks a file's tokens instead of deleting them, and the difference is not cosmetic: the
/// parse that follows maps its declarations back through the **original** span table, so a stream that came out
/// six kilobytes shorter moved every later offset by six kilobytes and filed every declaration after it under the
/// wrong file. Measured on a project that includes `<string>`: `basic_string`'s 93 971-byte class body — the one
/// written in `<xstring>` — was filed under `<__msvc_formatter.hpp>`, whose *forward declaration* of the same name
/// stands 6 100 bytes earlier. Nothing downstream could notice: the name was right, the scope was right, and the
/// range was right for the wrong file.
///
/// Three properties, and each one is what the mapping needs:
///
/// ```text
/// the text is the same length          so an offset in the result is an offset in the original
/// the spans are the original ones      so `written_span` answers about the text that is still there
/// the token is gone from the *parse*   so the file contributes nothing but spaces
/// ```
#[test]
fn taking_a_file_out_of_the_stream_keeps_the_offsets() {
    let files = [
        ("/p/first.h", "struct First { int a; };\n"),
        ("/p/leaky.h", "struct Leaky { int b; };\n"),
        ("/p/third.h", "struct Third { int c; };\n"),
        (
            "/p/main.cpp",
            "#include \"first.h\"\n#include \"leaky.h\"\n#include \"third.h\"\nint use = 1;\n",
        ),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);
    let shared = timeline.definitions();
    let whole = timeline.cook_the_unit(&unit.sources, &shared, None, true);

    let leaky = whole
        .files
        .iter()
        .position(|path| path == Path::new("/p/leaky.h"))
        .expect("the file is in the unit") as u32;
    let mut left_out = std::collections::BTreeSet::new();
    left_out.insert(leaky);
    let without = whole.without(&left_out);

    assert_eq!(
        without.text.len(),
        whole.text.len(),
        "the text is the same length, which is what keeps every offset where it was"
    );
    assert_eq!(
        without.text.find("Third"),
        whole.text.find("Third"),
        "so a declaration after the left-out file is found at the same place"
    );
    assert!(
        !without.text.contains("Leaky"),
        "and the left-out file's own tokens are gone: {}",
        without.text
    );

    // **The mapping still answers about the text that is there**, which is the property the index depends on.
    let third = without.text.find("Third").expect("still in the stream");
    let (file, written) = without
        .written_span(cpp_parser::SourceRange::new(third, 5))
        .expect("mapped");
    assert_eq!(
        without.file_of(file),
        Some(Path::new("/p/third.h")),
        "a token after the left-out file still belongs to its own file"
    );
    assert_eq!(
        written.start_offset,
        "struct Third { int c; };\n".find("Third").expect("its own text"),
        "and to its own offset in that file, not to where it stands in the stream"
    );

    // …and the program without it still parses.
    let tree = cpp_parser::CppParser::parse(&without.text, cpp_parser::ParserConfig::default());
    assert!(tree.get_errors().is_empty(), "{:?}", tree.get_errors());
}

/// **A file the caller has no text for is a hole, not an empty file.**
///
/// The two produce different programs, and a census that conflated them would report "the unit parses" about a
/// unit it never read. The hole is counted, the files it named are still stitched (the walk reached them through
/// it), and nothing is invented for the missing file.
#[test]
fn a_file_without_text_is_a_hole_in_the_unit() {
    let files = [
        ("/p/config.h", "#define WIDTH 4\nstruct Cfg { int a; };\n"),
        (
            "/p/api.h",
            "#include \"config.h\"\nstruct Api { int a[WIDTH]; };\n",
        ),
        ("/p/main.cpp", "#include \"api.h\"\n"),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);
    let shared = timeline.definitions();

    // The same unit, with `api.h`'s text withheld — the shape of an editor whose buffer for it is not loaded.
    let mut partial: HashMap<PathBuf, String> = unit.sources.clone();
    partial.remove(Path::new("/p/api.h"));

    let stitched = timeline.cook_the_unit(&partial, &shared, None, true);
    assert_eq!(stitched.missing, 1, "api.h was reached and not read");
    assert!(
        !stitched.text.contains("Api"),
        "nothing was invented for the missing file: {}",
        stitched.text
    );
    assert!(
        stitched.text.contains("Cfg"),
        "and the file *it* included is still stitched, because the walk reached it through the hole: {}",
        stitched.text
    );
}

/// **A macro that declares a type is a declaration in the cooked reading, and points at the invocation.**
///
/// This is the whole reason the index reads the cooked stream: `DECLARE_HANDLE(HWND)` declares `HWND__` and
/// `HWND` to a compiler and *nothing* to a reader of the file's own text, because the declaration is inside the
/// 3 250 declaration names that exist only after expansion — `DECLARE_HANDLE`'s generated structs and members,
/// and — the larger half — the *scopes*: every declaration between `_STD_BEGIN` and `_STD_END` is `std::`-qualified
/// in the cooked reading and at file scope in the raw one, which is the difference between a lookup that finds
/// `std::to_string` and one that does not.
#[test]
fn a_macro_that_declares_a_type_declares_it_in_the_cooked_reading() {
    let declaration = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                       typedef struct name##__ *name\n";
    let files = [
        ("/p/decl.h", declaration),
        ("/p/api.h", "#include \"decl.h\"\nDECLARE_HANDLE(HWND);\n"),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/api.h", &mut definitions);
    let shared = timeline.definitions();
    let config = CompilerConfig::default();
    let indexer = FileIndexer::new(&unit.files, &config);
    let key = SummaryKey::new(0, 0);

    let api = Path::new("/p/api.h");
    let source = unit.sources.get(api).expect("read");

    // **The raw reading**: the file's own text. `DECLARE_HANDLE(HWND);` is an invocation, and the declaration it
    // stands for is inside `decl.h`'s `#define`, where nothing this file declares lives.
    let raw = indexer.index(api, source, key);
    let raw_names: Vec<&str> = raw.declarations.iter().map(|fact| &*fact.name).collect();
    assert!(
        !raw_names.contains(&"HWND__") && !raw_names.contains(&"HWND"),
        "the raw reading cannot see a declaration that is inside a macro body: {raw_names:?}"
    );

    // **The cooked reading**: the program a compiler sees, indexed and mapped back into the file.
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let macros = cpp_code_analysis::FileMacros::new(
        timeline.environment_of(api).expect("the unit reaches api.h"),
        &shared,
        None,
        true,
    );
    let cooked = cpp_code_analysis::cook_with(source, &tokens, &macros);
    let rendered = cooked.render();
    assert!(
        rendered.text.contains("HWND__"),
        "the rendering has the declaration: {}",
        rendered.text
    );

    let indexed = indexer.index_rendering(api, &rendered, key);
    let cooked_summary = &indexed.summary;
    let report = indexed.mapped;
    let cooked_names: Vec<&str> = cooked_summary
        .declarations
        .iter()
        .map(|fact| &*fact.name)
        .collect();
    assert!(
        cooked_names.contains(&"HWND__"),
        "the struct the macro declares is a declaration here: {cooked_names:?}"
    );
    assert!(
        cooked_names.contains(&"HWND"),
        "…and the typedef beside it: {cooked_names:?}"
    );
    assert_eq!(report.dropped, 0, "every range landed in the file");
    assert!(report.placed > 0);

    // **And the range points at the invocation**, which is the only place in this file a reader can act on: the
    // declaration's text is in `decl.h`, and `map_into_the_file` reports the call site for exactly that reason.
    let hwnd = cooked_summary
        .declarations
        .iter()
        .find(|fact| fact.name == "HWND__")
        .expect("declared");
    let written = &source[hwnd.range.start_offset..hwnd.range.end_offset()];
    assert!(
        written.contains("DECLARE_HANDLE"),
        "the declaration is reported at the invocation: {written:?}"
    );
}

/// **A definition knows which file it was written in.**
///
/// A `MacroDef` used to be ranges and nothing else, which is a lie in two of the three places a definition can
/// come from: one read from a file is in that file, one that arrived from another file of a walked unit is in
/// *that* file, and one reconstructed from text alone is in no file at all. The map back to files is what made
/// the difference visible — a body token of an inherited macro was reported at its offset in the
/// reconstruction, a position in no file, which a consumer then slices into the file it has.
///
/// This test walks the whole path: the unit reads a definition out of `config.h`, `api.h` invokes it, the source
/// file invokes *that*, and the token the reader ends up with says where each hop was written.
#[test]
fn a_definition_says_which_file_it_was_written_in() {
    let files = [
        ("/p/config.h", "#define WIDTH 4\n"),
        ("/p/api.h", "#include \"config.h\"\n#define API WIDTH\n"),
        ("/p/main.cpp", "#include \"api.h\"\nint v = API;\n"),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);
    let shared = timeline.definitions();

    let main = Path::new("/p/main.cpp");
    let text = unit.sources.get(main).expect("read");
    let (tokens, _) = cpp_parser::lex(text, &cpp_parser::LexerConfig::default());
    let macros = cpp_code_analysis::FileMacros::new(
        timeline.environment_of(main).expect("the unit reaches main.cpp"),
        &shared,
        None,
        true,
    );
    let cooked = cpp_code_analysis::cook_with(text, &tokens, &macros);
    let rendered = cooked.render();

    // The `4`: written in `config.h`'s `#define WIDTH 4`, pasted by `api.h`'s `#define API WIDTH`, invoked in
    // main.cpp.
    let four = cooked
        .tokens
        .iter()
        .find(|token| token.text() == "4")
        .expect("the expansion is in the stream");
    let at = rendered.text.find(" 4 ").expect("the expansion is in the rendering");

    // **Where it was written is not this file**, and the map says so rather than handing over an offset that
    // belongs to a reconstruction.
    assert_eq!(
        rendered.written_at(at + 1),
        None,
        "the spelling of `4` is in config.h, not in main.cpp"
    );
    // **Where to report it is this file**: the invocation the reader can see.
    let reported = rendered.reported_at(at + 1).expect("a place to report");
    assert_eq!(
        &text[reported.start_offset..reported.end_offset()],
        "API",
        "a diagnostic lands on the call site"
    );

    // **Navigation lands in the right file**: the innermost definition that can be pointed at is `WIDTH`, and it
    // is in `config.h` — a consumer turns the frame into a path and opens it.
    let (file, name_at) = four.navigation_at().expect("a definition in a file");
    let config = Path::new("/p/config.h");
    let cpp_code_analysis::macros::MacroFile::Frame(frame) = file else {
        panic!("`WIDTH` was not written in the file being cooked: {file:?}");
    };
    assert_eq!(
        timeline.frame_file(frame),
        Some(config),
        "the frame names config.h"
    );
    let config_text = unit.sources.get(config).expect("read");
    assert_eq!(
        &config_text[name_at.start_offset..name_at.end_offset()],
        "WIDTH",
        "and the range is the macro's name, in that file"
    );

    // **A macro the file itself wrote is in the file itself**: the other side of the same question, on one
    // fixture, so that "the map says another file" cannot pass by saying it everywhere.
    let local = [
        ("/p/local.cpp", "#define HERE 7\nint v = HERE;\n"),
    ];
    let unit = Unit::new(&local);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/local.cpp", &mut definitions);
    let shared = timeline.definitions();
    let path = Path::new("/p/local.cpp");
    let text = unit.sources.get(path).expect("read");
    let (tokens, _) = cpp_parser::lex(text, &cpp_parser::LexerConfig::default());
    let macros = cpp_code_analysis::FileMacros::new(
        timeline.environment_of(path).expect("reached"),
        &shared,
        None,
        true,
    );
    let cooked = cpp_code_analysis::cook_with(text, &tokens, &macros);
    let rendered = cooked.render();
    let seven = cooked
        .tokens
        .iter()
        .find(|token| token.text() == "7")
        .expect("the expansion");
    let at = rendered.text.find(" 7 ").expect("in the rendering") + 1;
    let written = rendered.written_at(at).expect("written in this file");
    assert_eq!(
        &text[written.start_offset..written.end_offset()],
        "7",
        "a body token of a file's own macro is at the body, in this file"
    );
    assert_eq!(
        seven.navigation_at().expect("a definition in a file").0,
        cpp_code_analysis::macros::MacroFile::Here
    );
}

/// **A file cooks the same through the unit as through a table of its own.**
///
/// The one-walk path stopped building a table per file: it reads the unit's definitions **once**
/// ([`TranslationUnit::definitions`]) and answers each query positionally (`FileMacros`). That is a different
/// mechanism for the same question, and the only thing that matters about it is that the answer is the same — a
/// binding that is in force in one reading and not in the other changes what the cooker pastes, which shows up
/// much later as a parse that reads something else.
///
/// So this test cooks every file of a fixture **twice**: once from the shared unit, once from the table the
/// per-file reading built, and compares the rendered text byte for byte. The fixture is chosen to put every layer
/// of the new state under the comparison: a definition inside a taken `#if` (the in-force channel, which is the
/// newest layer), a conditional definition the seed decides, an `#undef` in the middle of a file (which must
/// shadow what the include brought in from that offset on), and a name the seed also carries.
#[test]
fn a_file_cooks_the_same_through_the_unit_as_through_a_table_of_its_own() {
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
             #define OBJECT 1\n\
             #endif\n",
        ),
        (
            "/p/api.h",
            "#include \"config.h\"\n\
             #define LATE(x) (x)\n\
             int f = OBJECT + LATE(2);\n\
             #undef OBJECT\n\
             int g = SA(GG);\n",
        ),
        (
            "/p/main.cpp",
            "#include \"api.h\"\n\
             #define LATE(x) [x]\n\
             int h = OBJECT;\n\
             LATE(3)\n",
        ),
    ];

    let unit = Unit::new(&files);
    let mut definitions = MacroDefinitions::default();
    let timeline = unit.timeline_of("/p/main.cpp", &mut definitions);

    // The unit's definitions, read once — what replaced the per-file table build.
    let shared = timeline.definitions();

    let mut rendered_something_the_reading_can_be_told_apart_by = false;

    for (path, text) in &files {
        let path = Path::new(path);
        let view = timeline
            .environment_of(path)
            .unwrap_or_else(|| panic!("the unit reaches {}", path.display()));

        // The reading that builds a table for this file…
        let mut per_file_definitions = cpp_code_analysis::ParsedDefinitions::new();
        let configuration =
            cpp_code_analysis::configuration_from_environment_and(&view, true, &mut per_file_definitions);

        // …and the reading that reads the unit once and answers positionally.
        let file_macros = cpp_code_analysis::FileMacros::new(view, &shared, None, true);

        let (tokens, _) = cpp_parser::lex(text, &cpp_parser::LexerConfig::default());
        let from_the_table = cpp_code_analysis::cook_with(text, &tokens, &configuration.table);
        let from_the_unit = cpp_code_analysis::cook_with(text, &tokens, &file_macros);

        // The precondition of the comparison, asserted rather than assumed: this file really does expand
        // something from outside itself, so "they agree" is not "both rendered an empty stream". (A header of
        // nothing but directives renders to nothing, and that is correct — `config.h` is exactly that.)
        rendered_something_the_reading_can_be_told_apart_by |= !from_the_table.render().text.trim().is_empty();

        assert_eq!(
            from_the_unit.render().text,
            from_the_table.render().text,
            "{} cooked differently through the unit than through its own table",
            path.display()
        );
    }

    assert!(
        rendered_something_the_reading_can_be_told_apart_by,
        "the fixture rendered nothing at all, so the comparison above was vacuous"
    );

    // **The seed is the oldest layer**, and the order is the whole of what makes the shared state equal to the
    // table it replaced: a name the unit defines wins over the compiler's own, a name the unit says nothing about
    // still comes from the seed, and a name the unit has **said something else** about is settled by that and not
    // by the seed.
    //
    // Asked of `main.cpp`'s view, and that is the point rather than a detail: `config.h`'s own `#define`s are not
    // in `config.h`'s environment — a file does not inherit its own facts, which is the contract
    // `TranslationUnit::environment_at` documents and the parser relies on (it reads the file's own `#define`s out
    // of the text). `main.cpp` includes `api.h`, so for *it* those facts are the unit's.
    let builtins = "#define LATE 99\n#define OBJECT 99\n#define _FROM_THE_SEED_ 1\n";
    let (tokens, _) = cpp_parser::lex(builtins, &cpp_parser::LexerConfig::default());
    let seed = cpp_code_analysis::preprocess::preprocess(builtins, &tokens).macros;

    let body_of = |definition: &cpp_code_analysis::macros::MacroDef| -> String {
        definition
            .body
            .significant()
            .map(|token| token.text().to_string())
            .collect()
    };
    let with_seed = cpp_code_analysis::FileMacros::new(timeline.root_environment(), &shared, Some(&seed), true);

    assert_eq!(
        with_seed.definition("LATE").map(&body_of),
        Some("(x)".to_string()),
        "api.h's `#define LATE(x) (x)` arrives through the include and is in force over the seed's `99` \
         (main.cpp's own `LATE(x) [x]` is the *own* layer, which the cook adds — not this one)"
    );
    assert!(
        with_seed.definition("_FROM_THE_SEED_").is_some(),
        "a name only the seed defines still reaches the cook — that is what the oldest layer is for"
    );
    // **A gap this round did not change, asserted so that it stays visible.** api.h `#undef`s `OBJECT`, and the
    // unit knows it — but the cook's table has no way to say "not defined": an entry that said so would have to
    // shadow the seed, and the environment's `#undef` facts are not entries at all (an `#undef` is not a
    // definition, which is what `for_each_definition` yields). So the seed's `99` comes back. It did before this
    // round too — the per-file table had the same hole for the same reason, and the readings are byte-identical —
    // so this is a *known gap* rather than a behaviour this round introduced. Fixing it means a third state in
    // the layer ("certainly not defined here"), which is worth doing on purpose and with its own measurement.
    assert_eq!(
        with_seed.definition("OBJECT").map(&body_of),
        Some("99".to_string()),
        "the unit's `#undef` does not reach the cook's table — see the note above"
    );
    assert!(
        with_seed.definition("_NOBODY_SAYS_").is_none(),
        "and a name nobody defines is nobody's: the seed does not invent one"
    );
}
