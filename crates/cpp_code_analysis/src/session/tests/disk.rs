//! **Disk** — opening a project on disk

use crate::file::paths::DiskFiles;
use crate::include::config::CompilerConfig;
use crate::index;
use crate::index::StepOutcome;
use crate::index::watch::WatchFilter;
use crate::parse_compile_commands;
use crate::session::{OpenDocuments, Session, SessionFiles};
use crate::symbol::Known;
use std::path::PathBuf;

use super::*;

// -------------------------------------------------------------------------------------------
// Opening a project on disk
// -------------------------------------------------------------------------------------------

/// A project directory, removed when the test ends — including when it fails.
pub(crate) struct Project {
    pub(crate) root: PathBuf,
}

impl Project {
    pub(crate) fn new(name: &str) -> Self {
        let root = a_fresh_fixture_directory("cppls-session-disk", name);
        std::fs::create_dir_all(&root).expect("the fixture directory");

        Project { root }
    }

    pub(crate) fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the fixture directory");
        }
        std::fs::write(&path, text).expect("the fixture writes");
        path
    }

    /// The project's own files, as paths relative to the root with `/` separators.
    pub(crate) fn relative(&self, paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .filter_map(|path| path.strip_prefix(&self.root).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
pub(crate) fn the_scan_finds_the_projects_sources_and_leaves_the_rest() {
    // What a scan is for, and what it must not do: a build tree and `.git` are not project source, a `.md` is
    // not a source at all, and a file with no extension is reached through an include rather than guessed at.
    let project = Project::new("scan");
    project.write("main.cpp", "int main() { return 0; }\n");
    project.write("include/widget.hpp", "struct Widget { int size; };\n");
    project.write("notes.md", "# notes\n");
    project.write("build/generated.cpp", "int generated;\n");
    project.write(".git/hooks/thing.cpp", "int hook;\n");
    project.write("extensionless", "int nothing;\n");

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let filter = WatchFilter::new(&project.root).ignore(project.root.join("build"));

    let session = Session::with_config(
        &project.root,
        providers.clone(),
        filter,
        CompilerConfig::default(),
    );

    assert_eq!(
        project.relative(session.project_files()),
        ["include/widget.hpp", "main.cpp"]
    );
    assert_eq!(
        session.pending(),
        2,
        "and the list is the queue's seed, so the work is ready before the first query"
    );
}

// **The ask this test needed is here** — `index_everything` reads a file through its timeline, so a class behind
// another file's macro body is scoped by the session (the defect and its reproducer are in `docs/open-questions.md`
// §6, and `a_keystroke_in_a_body_reads_the_file_through_its_timeline` pins the keystroke half).
/// **Two fixtures with one name get two directories** — the state that made a failing run keep failing.
///
/// The sessions in this file write their summaries under `<fixture>/.cppls`, and the fixtures used to share one tree per
/// name. [`std::fs::remove_dir_all`]'s failure was ignored, so a directory that could not be removed was read as the new
/// run's state: a summary under the name of a file the fixture had just rewritten, answered with the old reading, and
/// nothing in the run could clear it. The name now carries the process id and a serial, which is what this pins.
#[test]
pub(crate) fn a_fixture_directory_is_unique_to_its_run() {
    let first = a_fresh_fixture_directory("cppls-session-tests", "shared-name");
    let second = a_fresh_fixture_directory("cppls-session-tests", "shared-name");
    assert_ne!(
        first, second,
        "two fixtures of one name never share a directory"
    );

    // **And a directory the name happens to match is not read**: the fixture's own directory is a new one, so a stale
    // `.cppls` beside it (or in it) belongs to nobody.
    std::fs::create_dir_all(first.join(".cppls")).expect("the stale cache");
    std::fs::write(first.join(".cppls").join("stale"), b"an older reading")
        .expect("the stale cache");
    let third = a_fresh_fixture_directory("cppls-session-tests", "shared-name");
    assert_ne!(third, first, "a stale directory is not reused");
    assert!(
        !third.join(".cppls").exists(),
        "and its cache is not handed over"
    );
    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&third);
}

// **The ask, served by the next pump.** `index_everything` reads a file through its timeline only after the
// refinement ask for it has been served, and whether that happens before the caller asks its question is an ordering
// this test cannot control — so the test no longer tries to. What it asserts now is the contract the ask exists for:
// **a miss asks for the file that would have answered, and the next pump makes the answer exist.** That is
// `ProjectIndex::ask_for_refinement`, drained by `Session::advance`.
#[test]
pub(crate) fn a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session() {
    // The store's second pass, on the closure a **session** builds one file at a time. MSVC's STL is
    // written this way: `<vector>` opens `std` with `_STD_BEGIN`, whose replacement list is in `yvals_core.h`,
    // and it *includes* that file — while a file is read before the files it includes, because its own
    // `#include`s are a product of parsing it. So the first reading of every such file puts its declarations at
    // file scope. Measured on the whole STL through the driver: **0/9** without this pass, **9/9** with it,
    // against 9/9 for the same index built by `SummaryStore::index_includes_from` (which always had one).
    //
    // No compiler and no compile database take part: the body is written in a file of the project, which is
    // what makes this test the same on every machine.
    let project = Project::new("body-pass");
    let open = project.write(
        "open.h",
        "#pragma once\n#define _STD_BEGIN namespace std {\n#define _STD_END }\n",
    );
    let widget = project.write(
        "widget.h",
        "#include \"open.h\"\n_STD_BEGIN\nstruct Widget { int size; };\n_STD_END\n",
    );
    let main = project.write("main.cpp", "#include \"widget.h\"\n");

    let mut session = Session::with_config(
        &project.root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );
    session.index_everything();

    // **The first question does not know, and says so** — the name is qualified, its bare form exists at file scope,
    // and that is exactly the shape that asks for `widget.h` to be refined.
    let first = session.index().definition("std::Widget", &main);
    assert!(
        !matches!(first, Known::Yes(_)),
        "the first request cannot answer from a header read without its macro body: {first:?}"
    );

    // **And the next pump serves the ask.** This is the whole contract; nothing about *when* the pump runs is
    // asserted, only that asking is what makes the second answer exist.
    session.index_everything();

    let found = session.index().definition("std::Widget", &main);
    let Known::Yes(found) = found else {
        panic!("`Widget` is declared behind a macro body written in `open.h`: {found:?}");
    };
    assert_eq!(found.file, widget);
    assert_eq!(found.fact.scope.as_deref(), Some("std"));

    // And the scope is a reading **of the body, not a conclusion about it**. The body is in `open.h`; the scope it
    // opens is in `widget.h` only because `widget.h` renders through it. So a changed body has to be a changed
    // answer — and that, rather than a stored note, is what "evidence" means once the parser reads the rendering:
    // there is no scope read out of a replacement list to record (`tests/rendering_scope.rs` prints the rendering:
    // `namespace std {` is literal text there), and what makes the answer follow the body is that the summary *is*
    // the rendering, which [`Session::invalidate_dependents`] drops when a directive of an included file moves.
    assert_eq!(
        session.index().dependents_of(&open).len(),
        2,
        "`widget.h` includes it and `main.cpp` includes that: the index knows both"
    );
    session.did_change(
        &open,
        "#pragma once\n#define _STD_BEGIN namespace studio {\n#define _STD_END }\n",
    );
    assert!(
        !session.is_indexed(&widget),
        "the body moved, so the file that rendered through it has to be read again"
    );
    session.index_everything();

    assert!(
        matches!(
            session.index().definition("std::Widget", &main),
            Known::No | Known::Unknown(_)
        ),
        "the body no longer opens `std`, so nothing may still answer `std::Widget`"
    );
    // **The same two-step contract as above**, and for the same reason: the file was re-read without its macro body,
    // so the first question about the new spelling misses and asks. `studio::Widget` is the ask's answer.
    let first = session.index().definition("studio::Widget", &main);
    assert!(
        !matches!(first, Known::Yes(_)),
        "the re-read file is not scoped yet: {first:?}"
    );
    session.index_everything();
    let moved = session.index().definition("studio::Widget", &main);
    let Known::Yes(moved) = moved else {
        panic!("`Widget` is behind `namespace studio` now: {moved:?}");
    };
    assert_eq!(
        moved.file.file_name(),
        widget.file_name(),
        "the declaration is still `widget.h`'s"
    );
}

/// **A keystroke in a body rebuilds the file's summary *through its timeline*, not from its bytes.**
///
/// The window this closes is the one the four patch layers exist for. An edit drops the file's summary
/// ([`Session::did_change`]) and the next drain rebuilds it; while that rebuild was a parse of the file's own bytes,
/// every answer about the file was the *written* reading until the idle pass got to it — a scope opened by another
/// file's macro was `None`, a return type split by `#if` was the wrong branch. A body edit moves no directive, so the
/// timeline is still in force, and an index step that holds one now renders instead of parsing text.
#[test]
pub(crate) fn a_keystroke_in_a_body_reads_the_file_through_its_timeline() {
    let project = Project::new("keystroke-renders");
    project.write(
        "open.h",
        "#pragma once\n#define _STD_BEGIN namespace std {\n#define _STD_END }\n",
    );
    let widget = project.write(
        "widget.h",
        "#include \"open.h\"\n_STD_BEGIN\nstruct Widget { int size; };\n_STD_END\n",
    );

    let mut session = Session::with_config(
        &project.root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );
    session.add_project_files([widget.clone()]);
    // **The ask**: the reading asserted below is what a *request* produces.
    session.want_refinement(&widget);
    session.index_everything();
    // **The reading is the cooked one, not the summary.** A summary is the file's environment-free reading
    // (`docs/open-questions.md`), so `_STD_BEGIN` — a body from `open.h` — is not in it; the reading the program
    // implies is the file's cooked reading, and that is where a macro-opened scope lives.
    assert!(
        session
            .index()
            .cooked_reading(&widget)
            .is_some_and(|cooked| {
                cooked
                    .declarations
                    .iter()
                    .any(|fact| fact.name == "Widget" && fact.scope.as_deref() == Some("std"))
            }),
        "the first reading is the timeline's, from the second pass"
    );

    // **The keystroke**: a declaration added to the body. No directive moves, so the timeline still describes this
    // file — which is the case the pump sees on most keystrokes.
    session.did_change(
        &widget,
        "#include \"open.h\"\nint padding;\n_STD_BEGIN\nstruct Widget { int size; };\n_STD_END\n",
    );
    let steps = session.advance(4);
    let step = steps
        .iter()
        .find(|step| step.path == widget)
        .unwrap_or_else(|| panic!("the edited file is read: {:?}", paths(&steps)));
    assert!(
        step.with_the_environment,
        "the rebuild was taken through the file's timeline, so the second pass has nothing to repair: {step:?}"
    );

    // And the facts say so: the scope of a declaration written inside a macro-opened namespace is `std` **now**,
    // before any idle pass could have re-read the file.
    let cooked = session
        .index()
        .cooked_reading(&widget)
        .expect("read again, in its program");
    let widget_fact = cooked
        .declarations
        .iter()
        .find(|fact| fact.name == "Widget")
        .expect("the class is declared");
    assert_eq!(
        widget_fact.scope.as_deref(),
        Some("std"),
        "a rendering, not the file's own bytes"
    );
}

/// **A file opened after the project was indexed is read through a timeline that has to be *walked*, not one in hand.**
///
/// The other half of the window [`a_keystroke_in_a_body_reads_the_file_through_its_timeline`] closes. That one
/// pins the cheap case — a body edit keeps the file's timeline — and the timeline table is bounded
/// ([`crate::session::MAX_UNIT_BYTES`], [`crate::session::MAX_UNITS`]), so after a project is indexed the file a reader opens has no
/// entry. The step then used to parse the file's own bytes and leave the repair to the idle pass.
///
///
/// ```text
/// directives   the file's includes, read without a parse (`SummaryStore::directive_summary`)
/// the unit     walked from them, and *refused* when a file the program needs has not been read
/// the parse    of the rendering, which is then the reading every consumer gets
/// ```
#[test]
pub(crate) fn a_file_opened_after_the_index_was_built_is_read_through_a_walked_timeline() {
    let project = Project::new("opened-renders");
    project.write(
        "open.h",
        "#pragma once\n#define _STD_BEGIN namespace std {\n#define _STD_END }\n",
    );
    let widget = project.write(
        "widget.h",
        "#include \"open.h\"\n_STD_BEGIN\nstruct Widget { int size; };\n_STD_END\n",
    );

    let mut session = Session::with_config(
        &project.root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );
    session.add_project_files([widget.clone()]);
    // **The ask**: the reading asserted below is what a *request* produces.
    session.want_refinement(&widget);
    session.index_everything();

    // **The edit that takes the timeline away.** A body edit keeps it ([`Session::directives_moved`] compares the
    // file's directives, and a body is not one); a directive edit moves the layout, which is a change to every offset
    // the timeline recorded — so the entry is dropped, the summary with it, and the file is queued at
    // `Priority::Open`. That is the state the units table's byte budget leaves a reader in after a project is
    // indexed, and it used to mean "read the file's own bytes".
    let edited = format!(
        "{}#define PAD 1\n",
        std::fs::read_to_string(&widget).expect("the fixture reads")
    );
    session.did_change(&widget, &edited);
    assert!(
        session.index().summary(&widget).is_none(),
        "the edit drops the reading: the buffer is the text now"
    );

    // **The walk is the thing being measured**, so it is counted rather than assumed: a timeline in hand would
    // answer this step with no walk at all, and then the test would be about the other one.
    let walked_before = session.unit_stats().walked;
    let steps = session.advance(4);
    let step = steps
        .iter()
        .find(|step| step.path == widget)
        .unwrap_or_else(|| panic!("the opened file is read: {:?}", paths(&steps)));
    assert!(
        session.unit_stats().walked > walked_before,
        "the timeline was walked for this file: {:?}",
        session.unit_stats()
    );
    assert!(
        step.with_the_environment,
        "and the reading taken through it is the program's, so the second pass has nothing to repair: {step:?}"
    );

    let summary = session.index().summary(&widget).expect("indexed again");
    let widget_fact = summary
        .declarations
        .iter()
        .find(|fact| fact.name == "Widget")
        .expect("the class is declared");
    assert_eq!(
        widget_fact.scope.as_deref(),
        Some("std"),
        "the scope comes from another file's macro body, so the timeline is what answered"
    );
}

/// **A file whose program nobody has read yet is read as text, and that is the honest answer rather than a worse one.**
///
/// The guard on the walk above, and the reason it is a condition rather than a retry: a timeline
/// that stops at a file it cannot look up is a timeline of a *smaller* program — the macro is left
/// unexpanded in the rendering — and a rendering is written down and believed.
#[test]
pub(crate) fn a_file_whose_program_has_not_been_read_yet_is_still_read_as_text() {
    let project = Project::new("opened-before-the-program");
    project.write(
        "open.h",
        "#pragma once\n#define _STD_BEGIN namespace std {\n#define _STD_END }\n",
    );
    let widget = project.write(
        "widget.h",
        "#include \"open.h\"\n_STD_BEGIN\nstruct Widget { int size; };\n_STD_END\n",
    );
    let text = std::fs::read_to_string(&widget).expect("the fixture reads");

    // **Nothing is read yet**: the session is told about the project and about the buffer, and no pass has run.
    let mut session = Session::with_config(
        &project.root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );
    session.add_project_files([widget.clone()]);
    session.did_open(&widget, &text);

    let steps = session.advance(4);
    let step = steps
        .iter()
        .find(|step| step.path == widget)
        .unwrap_or_else(|| panic!("the opened file is read: {:?}", paths(&steps)));
    assert!(
        !step.with_the_environment,
        "`open.h` has not been read, so there is no timeline to read this file through: {step:?}"
    );
    assert_eq!(
        step.outcome,
        StepOutcome::Built,
        "and the file's own text is what was read: {step:?}"
    );
}

/// **A program read files every file's reading under the file's own key.** The reading used to live only under the
/// program's id, so nothing could serve it afterwards: a second session re-read the whole program to get back facts
/// that had been computed and thrown away with the first. The disk record this tests is what
/// `take_the_stored_refinements` serves on a warm start, and what a per-file cook will ask for when the cache hit
/// lands.
#[test]
pub(crate) fn a_program_read_files_each_files_reading_on_the_disk() {
    let project = Project::new("program-writes-per-file");
    project.write("main.cpp", "#include \"api.h\"\nHWND h;\n");
    project.write("api.h", "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n");
    project.write(
        "handle.h",
        "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \\\n\
         typedef struct name##__ *name\n",
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let mut session = Session::with_config(
        &project.root,
        providers,
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );
    session.add_project_files([project.root.join("main.cpp")]);
    session.index_everything();

    let reading = session
        .read_the_unit(&project.root.join("main.cpp"))
        .expect("the program reads");
    assert_eq!(
        reading.files, 3,
        "the program is main.cpp, api.h and handle.h"
    );

    // **The record, under the included file's own key** — not under the program's, which is the in-session half.
    let api = project.root.join("api.h");
    let stored = session
        .store
        .the_stored_cooked_reading(&api)
        .expect("the program's reading of api.h is filed under api.h");
    let in_the_index = session
        .index()
        .cooked_declarations(&api)
        .expect("and in the index, under the program");
    assert_eq!(
        stored.declarations.len(),
        in_the_index.len(),
        "the disk record and the index hold one reading"
    );
    assert!(
        !stored.declarations.is_empty(),
        "the reading is the program's, so it has the macro's declaration"
    );
}

/// **A record written by one session is a cook the next session does not run.**
///
/// The per-file cook writes its facts *and* its rendering. A second session on the same project asks the disk first,
/// and the record answers: no lex, no render, no parse — and `view` answers immediately, because the rendering came
/// with it. The observable is the walk count: a session that has to build the environment walks a timeline, and one
/// that is served does not.
#[test]
pub(crate) fn a_stored_cook_answers_the_next_session() {
    let project = Project::new("a-stored-cook-answers");
    let source = "#include \"b.h\"\nint main() { return B; }\n";
    let main = project.write("main.cpp", source);
    project.write("b.h", "#define B 1\n");

    let open = || {
        Session::with_config(
            &project.root,
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&project.root),
            CompilerConfig::default(),
        )
    };

    // **Nothing on disk, so this session pays for the cook** and writes the record with its rendering. It pays with a
    // *composition*, not a timeline: the environment a file needs is a function of its text and the macros in force
    // where it is included, so cooking one file no longer walks its closure (`docs/open-questions.md`).
    let mut first = open();
    first.add_project_files([main.clone()]);
    first.did_open(&main, source);
    first.index_everything();
    assert!(
        first.index().cooked_declarations(&main).is_some(),
        "the first session cooked the file"
    );
    assert_eq!(
        first.unit_stats().walked,
        0,
        "and it composed the environment instead of walking one: {:?}",
        first.unit_stats()
    );

    // **A session that has never seen the project.** Only the disk record connects them.
    let mut second = open();
    second.add_project_files([main.clone()]);
    second.did_open(&main, source);
    second.index_everything();
    assert!(
        second.index().cooked_declarations(&main).is_some(),
        "the file is read in the second session"
    );
    assert_eq!(
        second.unit_stats().walked,
        0,
        "and no timeline was walked: the record answered with its rendering: {:?}",
        second.unit_stats()
    );
    assert!(
        second.view_of_the_file(&main).is_some(),
        "and the rendering is there for a view, which is what a facts-only record could not give"
    );
    assert!(
        second.cooks_served_from_disk() > 0,
        "and the counter the budget is read from says so: {} served",
        second.cooks_served_from_disk()
    );
}

/// **A comment edited into a header does not invalidate the records that include it.** The cooked record is keyed by
/// the macros in force, not by the closure's content hash: a comment changes the text and no macro, so the reading the
/// first session wrote is still the reading this environment produces — and the second session is served instead of
/// re-cooking. Under the closure fingerprint this was a miss, because the header's hash is inside it.
#[test]
pub(crate) fn a_comment_edit_to_a_header_keeps_the_cook_record() {
    let project = Project::new("a-comment-edit-keeps-the-record");
    let source = "#include \"api.h\"\nLive live;\n";
    let main = project.write("main.cpp", source);
    project.write("api.h", "// the first comment\nstruct Live { int a; };\n");

    let open = || {
        Session::with_config(
            &project.root,
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&project.root),
            CompilerConfig::default(),
        )
    };

    // **The run that writes the record.**
    {
        let mut first = open();
        first.add_project_files([main.clone()]);
        first.did_open(&main, source);
        first.index_everything();
        assert!(
            first.index().cooked_declarations(&main).is_some(),
            "the first session cooked the file"
        );
    }

    // **The header's comment changes, and nothing else.**
    project.write(
        "api.h",
        "// a longer comment, and the same declaration\nstruct Live { int a; };\n",
    );

    let mut second = open();
    second.add_project_files([main.clone()]);
    second.did_open(&main, source);
    second.index_everything();
    assert!(
        second.cooks_served_from_disk() > 0,
        "a comment is not a change to the macros in force, so the record still answers: {} served",
        second.cooks_served_from_disk()
    );
}

/// **A header that appears earlier on the search path moves the include.**
///
/// `SummaryStore::where_it_resolves_now` remembers each search for the life of the store, and
/// `resolution_still_holds` uses those remembered answers to decide whether a stored summary is still current. A
/// header created mid-session makes both wrong in the same direction: the *fresh* search would find the new file, but
/// the remembered answer — the old file — matches what the summary recorded, so the summary is kept and the file whose
/// include should now resolve earlier keeps the old reading (`docs/worklog.md` P1-5).
///
/// The watcher does its part: `naming_files` asks for the includer to be re-read. The memo is what defeated it.
#[test]
pub(crate) fn a_header_that_appears_earlier_on_the_path_moves_the_include() {
    use crate::include::config::IncludePath;

    let project = Project::new("include-order");
    project.write("a.cpp", "#include \"x.h\"\nvoid f() { X x; }\n");
    project.write("second/x.h", "struct X { int from_the_second; };\n");

    let mut config = CompilerConfig::default();
    config
        .include_paths
        .push(IncludePath::user(project.root.join("first")));
    config
        .include_paths
        .push(IncludePath::user(project.root.join("second")));

    let providers = SessionFiles::new(OpenDocuments::new(), DiskFiles);
    let mut session = Session::with_config(
        &project.root,
        providers,
        WatchFilter::new(&project.root),
        config,
    );
    let a_cpp = project.root.join("a.cpp");
    session.add_project_files([a_cpp.clone()]);
    session.index_everything();

    let resolved = |session: &Session<_>| -> String {
        session
            .index()
            .summary(&a_cpp)
            .and_then(|summary| summary.includes.first().cloned())
            .and_then(|include| include.resolved)
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    };
    assert!(
        resolved(&session).ends_with("second/x.h"),
        "the only `x.h` is in the second directory: {:?}",
        resolved(&session)
    );

    // The header the search should have found first, created now.
    project.write("first/x.h", "struct X { int from_the_first; };\n");
    let answer = session.changed([index::watch::FileEvent::created(
        project.root.join("first/x.h"),
    )]);
    assert!(
        answer.reindex.iter().any(|path| path.ends_with("a.cpp")),
        "the file whose include can now resolve elsewhere is asked for: {:?}",
        answer.reindex
    );
    session.index_everything();

    assert!(
        resolved(&session).ends_with("first/x.h"),
        "the include resolves where the search order says it does, now that the file exists: {:?}",
        resolved(&session)
    );
}

/// **A second session does not trust a stored include search.**
///
/// The first session stored `#include "x.h"` resolving to `second/x.h`, and the search is also remembered in the
/// database so a later session does not repeat it. A header that then appears in `first/` — earlier on the path —
/// makes that remembered answer wrong, and nothing about the *summary's* key can notice: the text, the configuration
/// and the directory are all unchanged. The summary is a candidate and `resolution_still_holds` is what makes it an
/// answer, so the check has to ask the filesystem, not a recorded search (`docs/worklog.md` P1-5).
#[test]
pub(crate) fn a_second_session_does_not_trust_a_stored_include_search() {
    use crate::include::config::IncludePath;

    let project = Project::new("include-search-cache");
    project.write("a.cpp", "#include \"x.h\"\nvoid f() { X x; }\n");
    project.write("second/x.h", "struct X { int from_the_second; };\n");

    let config = {
        let mut config = CompilerConfig::default();
        config
            .include_paths
            .push(IncludePath::user(project.root.join("first")));
        config
            .include_paths
            .push(IncludePath::user(project.root.join("second")));
        config
    };
    let a_cpp = project.root.join("a.cpp");

    let open_and_read = || {
        let providers = SessionFiles::new(OpenDocuments::new(), DiskFiles);
        let mut session = Session::with_config(
            &project.root,
            providers,
            WatchFilter::new(&project.root),
            config.clone(),
        );
        session.add_project_files([a_cpp.clone()]);
        session.index_everything();
        session
    };
    let resolved = |session: &Session<_>| -> String {
        session
            .index()
            .summary(&a_cpp)
            .and_then(|summary| summary.includes.first().cloned())
            .and_then(|include| include.resolved)
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    };

    let first_session = open_and_read();
    assert!(
        resolved(&first_session).ends_with("second/x.h"),
        "{:?}",
        resolved(&first_session)
    );
    drop(first_session);

    // The header the search should find first, created while no session is open.
    project.write("first/x.h", "struct X { int from_the_first; };\n");

    let second_session = open_and_read();
    assert!(
        resolved(&second_session).ends_with("first/x.h"),
        "the stored search must not outvote the filesystem: {:?}",
        resolved(&second_session)
    );
}

/// **A source file's cached summary is validated with that file's own compilation.**
///
/// A compile command may put include directories ahead of the project-wide ones. The parser that wrote the summary
/// used those (`FileIndexer::scan_includes` asks `CompilerConfig::for_file`), but the cache check used to search with
/// the project-wide configuration. The summary recorded the source's path and the check found the project-wide one,
/// declared the resolution moved, and re-parsed the file on every start — the whole warm cache was defeated for every
/// translation unit that had its own include path.
#[test]
pub(crate) fn a_source_file_is_validated_with_its_own_compilation() {
    use crate::include::config::IncludePath;
    use std::sync::Arc;

    let project = Project::new("per-file-include");
    project.write("main.c", "#include \"x.h\"\nX x;\n");
    project.write("first/x.h", "struct X { int first; };\n");
    project.write("second/x.h", "struct X { int second; };\n");
    let root = project.root.to_string_lossy().replace('\\', "/");
    let database = parse_compile_commands(&format!(
        "[{{\"directory\": \"{root}\", \"file\": \"{root}/main.c\", \
          \"arguments\": [\"gcc\", \"-I{root}/first\", \"-c\", \"{root}/main.c\"]}}]"
    ));

    let mut config = CompilerConfig {
        root: Some(project.root.clone()),
        database: Some(Arc::new(database)),
        ..CompilerConfig::default()
    };
    config
        .include_paths
        .push(IncludePath::user(project.root.join("second")));

    let main = project.root.join("main.c");
    let open = || {
        let providers = SessionFiles::new(OpenDocuments::new(), DiskFiles);
        let mut session = Session::with_config(
            &project.root,
            providers,
            WatchFilter::new(&project.root),
            config.clone(),
        );
        session.add_project_files([main.clone()]);
        session.index_everything();
        session
    };

    let first = open();
    let resolved = |session: &Session<_>| -> String {
        session
            .index()
            .summary(&main)
            .and_then(|summary| summary.includes.first().cloned())
            .and_then(|include| include.resolved)
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    };
    assert!(
        resolved(&first).ends_with("first/x.h"),
        "the source's compile command comes first: {:?}",
        resolved(&first)
    );
    first.flush_the_summary_cache();
    drop(first);

    let second = open();
    assert_eq!(
        second.stats().rebuilt,
        0,
        "both the source and its header must be cache hits: {:?}",
        second.stats()
    );
}

/// **A wave prepared against one text is not committed against another.**
///
/// `prepare_a_wave` reads and parses a file; `commit_a_wave` applies the result and then records where the disk is
/// *now*. A file written between the two is the one state reconciliation can never leave: the index holds the reading of
/// the old text while the stamp describes the new one, so the next reconcile sees nothing to re-read and the stale facts
/// stay (`docs/worklog.md` P1-7).
#[test]
pub(crate) fn a_wave_prepared_against_one_text_is_not_committed_against_another() {
    let project = Project::new("wave-race");
    project.write(
        "a.cpp",
        "struct FromTheOldText { int one; };\nvoid f() { FromTheOldText old_value; }\n",
    );

    let providers = SessionFiles::new(OpenDocuments::new(), DiskFiles);
    let mut session = Session::with_config(
        &project.root,
        providers,
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );
    let a_cpp = project.root.join("a.cpp");
    session.add_project_files([a_cpp.clone()]);

    // The wave reads and parses the old text, and does not commit it.
    let wave = session.prepare_a_wave(4);

    // The disk moves before the commit.
    project.write(
        "a.cpp",
        "struct FromTheNewText { int two; };\nvoid f() { FromTheNewText new_value; }\n",
    );

    session.commit_a_wave(wave, 4);

    let names: Vec<String> = session
        .index()
        .summary(&a_cpp)
        .map(|summary| {
            summary
                .declarations
                .iter()
                .map(|fact| fact.name.to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        names.iter().any(|name| name == "FromTheNewText"),
        "the index holds the reading of the text on disk: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "FromTheOldText"),
        "and not the one the wave prepared: {names:?}"
    );
}
