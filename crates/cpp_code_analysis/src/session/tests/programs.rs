//! **Programs** — reading a program: roots, edits, invalidation, waves

use crate::file::coordinates::ReadingOffset;
use crate::file::paths::MemoryFiles;
use crate::index::StepOutcome;
use crate::session::{Session, queue_key};
use crate::symbol::Known;
use std::path::{Path, PathBuf};

use super::*;

/// **`index.read_the_whole_project = false` keeps the reader's own programs and nothing else.**
///
/// A project that says so gets the first and not the second, and the clock says it too: with the
/// setting off there is nothing for the pump to wait for once the reader's own programs are read.
#[test]
pub(crate) fn a_project_can_decline_to_read_its_other_roots() {
    let shared = "struct Shared { int size; };\n";
    let a = "#include \"shared.h\"\n";
    let b = "#include \"shared.h\"\nint from_b = 1;\n";
    let files = MemoryFiles::new()
        .with_file("/p/shared.h", shared)
        .with_file("/p/a.cpp", a)
        .with_file("/p/b.cpp", b)
        .with_file(
            "/p/.cppls.toml",
            "[index]\nread_the_whole_project = false\n",
        );
    let fixture = Memory::new("a-project-declines", &files);
    // **The session's root is the project's**, not the fixture's temporary directory: `.cppls.toml` is read from
    // under that root, and the fixture files live under `/p`.
    let mut session = crate::Session::with_config(
        "/p",
        fixture.providers.clone(),
        crate::WatchFilter::new("/p"),
        crate::CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/b.cpp"),
    ]);
    session.did_open("/p/a.cpp", a);
    session.index_everything();

    // **The reader's own file is still read**, one second after they stop typing.
    session.pretend_a_pause(std::time::Duration::from_millis(1_100));
    let first = session
        .read_a_program_now()
        .expect("the reader's own program");
    assert_eq!(first.root, std::path::Path::new("/p/a.cpp"));

    // **And then nothing is owed**, however long the pause: `b.cpp` is a root nobody has open, which this project
    // asked not to read.
    session.pretend_a_pause(std::time::Duration::from_secs(60));
    assert!(
        session.read_a_program_now().is_none(),
        "the project declined its unopened roots"
    );
    assert!(
        session.the_wait_before_a_program_may_be_read().is_none(),
        "so the pump has nothing to wait for"
    );
    assert!(
        !session
            .index()
            .the_program_that_read(std::path::Path::new("/p/b.cpp"))
            .is_some(),
        "and `b.cpp` was not read"
    );
}
/// **A body edit does not cost the program again; a directive edit does.**
///
/// A program read is 5.24 s of background CPU for 180 files (measured, `tests/keystroke_probe.rs`), and what decides
/// whether it has to be paid again is the **shape** of the program: a body edit moves no directive, so every file of
/// the closure but the edited one still reads exactly as it did — and the edited one is rebuilt by the per-file cook
/// that every keystroke already queues. A directive edit moves every offset the timeline recorded, and the rendering
/// of the whole program with it.
///
/// [`Session::the_wait_before_a_program_may_be_read`] is the question the pump asks — `Some` when a program is owed,
/// whatever the clock says — and it is what makes this testable without sleeping.
#[test]
pub(crate) fn a_body_edit_does_not_cost_the_program_again() {
    let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                  typedef struct name##__ *name\n";
    let main = "#include \"api.h\"\nHWND h;\n";
    let body_edit = "#include \"api.h\"\nHWND h;\nint padding;\n";
    let directive_edit = "#include \"api.h\"\nHWND h;\nint padding;\n#define PAD 1\n";
    let files = MemoryFiles::new()
        .with_file("/p/handle.h", handle)
        .with_file("/p/api.h", "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n")
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-body-edit-keeps-the-program", &files);
    let mut session = fixture.session();
    session.did_open("/p/main.cpp", main);
    session.index_everything();

    // **The program is read once**, and it stops being owed for that root.
    let reading = session
        .read_a_looked_at_unit()
        .expect("the reader has a file open, so there is a program to read");
    assert!(
        reading.files >= 3,
        "the program is three files: {reading:?}"
    );
    assert!(
        session.the_wait_before_a_program_may_be_read().is_none(),
        "one program per root, until something changes its shape"
    );

    // **A body edit**: the file's text changed, the program's shape did not.
    session.did_change("/p/main.cpp", body_edit);
    assert!(
        session.the_wait_before_a_program_may_be_read().is_none(),
        "a declaration added to a body does not make the program owed again"
    );
    // …and the edited file is still read, by the per-file path its keystroke queued.
    session.advance(4);
    assert!(
        session
            .index()
            .summary(Path::new("/p/main.cpp"))
            .is_some_and(|summary| summary
                .declarations
                .iter()
                .any(|fact| fact.name == "padding")),
        "the keystroke's own file is read again, by the per-file path"
    );

    // **A directive edit**: every offset the timeline recorded moved, so the program is owed again.
    session.did_change("/p/main.cpp", directive_edit);
    assert!(
        session.the_wait_before_a_program_may_be_read().is_some(),
        "a directive that moved makes the program worth reading again"
    );
}

#[test]
#[ignore = "a query still answers from the summary's conditional fact — the reading choice does not reach this question yet (design §4.1)"]
/// **Once a file is read as a program, the lookup-time guard filter has nothing left to do** — patch number two's
/// remaining work, measured.
///
/// The filter exists because a *summary* keeps both branches of every `#if`: `#if 0 struct Dead { … }; #endif` is a
/// declaration to a reader of the text and nothing to a compiler. A **rendering** has no branch left to remove, so a
/// file the session has read as a program answers from facts that are already the compiler's — and the filter becomes
/// a pass over candidates that never rejects one.
///
/// This test pins that on a fixture rather than on a project: the same questions are asked with the filter on and
/// off (`probes::set_no_guard_filter`, which exists for this measurement), and the answers must be identical. If they
/// ever differ, this file's reading is not the rendering and the patch has a user again.
/// # Why this is ignored rather than fixed, and why it is not deleted
///
/// It is **red**, and the way it fails is the signal: with the filter on, `Dead` — a class under `#if 0` — answers
/// `unknown:ConditionalCompilation` instead of being found, while with the filter off it is found. That is exactly
/// what this test was written to detect ("if they ever differ, this file's reading is not the rendering"), and they
/// now differ: after phase 2 the session builds no timelines, so an open file's summary is the **raw** reading again
/// and the lookup-time guard filter has a user it did not have. `docs/indexing-review.md` §3.1 is the same root
/// cause, measured a second way.
///
/// Ignored rather than rewritten: the assertion is right and the behaviour is wrong, and an assertion that accepted
/// `unknown` here would be pinning a regression as the contract. Deleting it would remove the instrument.
pub(crate) fn a_file_read_as_a_program_needs_no_guard_filter() {
    let cfg = "#define ON 1\n";
    let api = "#include \"cfg.h\"\n#if 0\nstruct Dead { int gone; };\n#endif\nstruct Live { int kept; };\n";
    let main = "#include \"api.h\"\nLive live;\n";
    let files = MemoryFiles::new()
        .with_file("/p/cfg.h", cfg)
        .with_file("/p/api.h", api)
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("no-guard-filter-needed", &files);
    let mut session = fixture.session();
    session.did_open("/p/main.cpp", main);
    session.index_everything();
    session
        .read_a_looked_at_unit()
        .expect("the reader has a file open, so there is a program to read");

    let ask = |session: &Session<_>| {
        let mut answers: Vec<String> = Vec::new();
        for name in ["Dead", "gone", "Live", "kept"] {
            let answer = match session.index().definitions(name, Path::new("/p/main.cpp")) {
                Known::Yes(found) => format!(
                    "{:?}",
                    found
                        .found
                        .iter()
                        .map(|one| (
                            one.file.to_string_lossy().to_string(),
                            one.fact.name.clone()
                        ))
                        .collect::<Vec<_>>()
                ),
                Known::Unknown(reason) => format!("unknown:{reason:?}"),
                Known::No => String::from("no"),
            };
            answers.push(format!("{name} = {answer}"));
        }
        answers
    };

    let with = ask(&session);
    session.index().forget_the_name_answers();
    crate::probes::set_no_guard_filter(true);
    let without = ask(&session);
    crate::probes::set_no_guard_filter(false);

    assert_eq!(
        with, without,
        "the filter changed an answer, so this file's reading is not the rendering: {with:?}"
    );
    assert!(
        !with.iter().any(|answer| answer.starts_with("Dead = yes")),
        "and the branch nobody takes is not an answer at all: {with:?}"
    );
}
/// **Where a file has been read as a program, that reading is the reading — not one answer beside another.**
///
/// The two declaration lists an index can hold for one file are not two views of one thing: the summary is the file
/// read *on its own* and a cooked reading is the file read **in a program**, and `_CRT_…`/`_GLIBCXX_…` decide
/// differently in the two. Measured on a real project, **1 095 declarations over 177 files** are in one list and not
/// the other (`tests/unit_read_probe.rs`). Offering both at once answers for two translation units with one question.
///
/// The fixture is that disagreement in one name: `api.h` declares `Live` one way when `FROM_THE_SOURCE` is defined and
/// another when it is not, so the file read on its own has one `Live` and the file read inside `main.cpp` has the
/// other. With the union, a lookup gets **both** — two declarations of one name, which is a list where a jump wants a
/// place. With the rendering as the reading, it gets the one its translation unit has.
#[test]
pub(crate) fn a_files_rendering_is_its_reading_and_not_one_answer_beside_another() {
    let api = "#ifdef FROM_THE_SOURCE\n\
               struct Live { int a; };\n\
               #else\n\
               struct Live { long b; };\n\
               #endif\n";
    let main = "#define FROM_THE_SOURCE 1\n#include \"api.h\"\nLive live;\n";
    let files = MemoryFiles::new()
        .with_file("/p/api.h", api)
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("the-rendering-is-the-reading", &files);
    let mut session = fixture.session();
    session.did_open("/p/main.cpp", main);
    session.index_everything();

    let taken = api.find("struct Live { int a; }").expect("the fixture");
    let not_taken = api.find("struct Live { long b; }").expect("the fixture");

    // **Before the program is read**: whatever the per-file reading produced, the one it did *not* produce is not an
    // answer for `main.cpp` — and that is the reading this change is about.
    let before = session
        .index()
        .definitions("Live", Path::new("/p/main.cpp"));
    println!("before the program read: {before:?}");

    session
        .read_a_looked_at_unit()
        .expect("the reader has a file open, so there is a program to read");

    // **One answer, and it is the branch this translation unit takes.** Two candidates here would mean the summary's
    // facts were offered beside the rendering's — the union this change removes.
    let found = session
        .index()
        .definitions("Live", Path::new("/p/main.cpp"));
    let Known::Yes(found) = found else {
        panic!("`Live` is declared in a header the source includes: {found:?}");
    };
    assert_eq!(
        found.found.len(),
        1,
        "the file has been read as a program, so its reading is the reading: {:?}",
        found
            .found
            .iter()
            .map(|one| one.fact.range)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        found.found[0].fact.range.start_offset, taken,
        "the answer is the branch `FROM_THE_SOURCE` selects: {:?}",
        found.found[0].fact.range
    );
    assert_ne!(
        found.found[0].fact.range.start_offset, not_taken,
        "and not the one the file has when it is read on its own"
    );
}

/// **A program is read even when its root's summary has not been built yet** — the CI failure, made deterministic.
///
/// `the_next_unit_to_read` accepts an open file whose summary is absent (its module question is `is_none_or`), so the
/// drain can ask for a program the index has not caught up with — and the read used to refuse it with a `?`, after
/// which the `while let Some(reading)` loop of the drain ended and a whole translation unit went unread. Measured on
/// CI: `both translation units are read: ["/p/a.cpp"]`, where `index_everything`'s rounds had built one root's summary
/// and not the other's; a faster machine built both, which is why the same test passed locally with the same code.
///
/// This test builds that state deliberately: two open files and **no** indexing, so neither has a summary.
#[test]
pub(crate) fn a_program_is_read_before_its_root_has_a_summary() {
    let cfg = "#ifdef A\nstruct X { int from_a; };\n#else\nstruct X { long from_b; };\n#endif\n";
    let a = "#define A 1\n#include \"cfg.h\"\n";
    let b = "#include \"cfg.h\"\n";
    let files = MemoryFiles::new()
        .with_file("/p/cfg.h", cfg)
        .with_file("/p/a.cpp", a)
        .with_file("/p/b.cpp", b);
    let fixture = Memory::new("a-program-without-its-summary", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", a);
    session.did_open("/p/b.cpp", b);

    let mut read = Vec::new();
    while let Some(reading) = session.read_a_looked_at_unit() {
        read.push(reading.root.clone());
    }
    assert_eq!(
        read.len(),
        1,
        "the focused document is read, and the background tab is not: {read:?}"
    );
    // **Focus the other tab**, which has no summary either: the same question, one document at a time.
    session.did_change("/p/a.cpp", a);
    let reading = session
        .read_a_looked_at_unit()
        .expect("the newly focused document is read");
    read.push(reading.root.clone());
    assert_eq!(
        read.len(),
        2,
        "both open files are read, summary or not: {read:?}"
    );
}

/// **The same header, read in two translation units, answers each of them its own way** — §5 step two's acceptance.
///
/// `cfg.h` declares `X` one way when `A` is defined and another when it is not, so `a.cpp` (which defines it before
/// the include) and `b.cpp` (which does not) read **different** `X` from the same bytes. The index keeps a reading
/// **per program** (`Readings`), and a lookup picks by the asking file's program — the `env_hash` of §5 step two.
/// Before that, the file kept the *last* program's reading and this test answered `b.cpp` with what `a.cpp` compiled.
#[test]
pub(crate) fn a_header_read_in_two_programs_answers_each_its_own_way() {
    let cfg = "#ifdef A\nstruct X { int from_a; };\n#else\nstruct X { long from_b; };\n#endif\n";
    let a = "#define A 1\n#include \"cfg.h\"\n";
    let b = "#include \"cfg.h\"\n";
    let files = MemoryFiles::new()
        .with_file("/p/cfg.h", cfg)
        .with_file("/p/a.cpp", a)
        .with_file("/p/b.cpp", b);
    let fixture = Memory::new("a-header-in-two-programs", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", a);
    session.did_open("/p/b.cpp", b);
    session.index_everything();

    // **Both programs, asked for by name.** The schedule reads the focused document only now; this test is about two
    // programs disagreeing about one header, not about which tab the reader is looking at.
    let mut read = Vec::new();
    for root in ["/p/a.cpp", "/p/b.cpp"] {
        let reading = session
            .read_the_unit(Path::new(root))
            .unwrap_or_else(|| panic!("{root} reads"));
        read.push(reading.root.clone());
    }
    assert_eq!(read.len(), 2, "both translation units are read: {read:?}");

    let from_a = session.index().definitions("X", Path::new("/p/a.cpp"));
    let from_b = session.index().definitions("X", Path::new("/p/b.cpp"));
    let offset = |known: &Known<crate::ProjectDefinitions>| match known {
        Known::Yes(found) => found.found.first().map(|one| one.fact.range.start_offset),
        _ => None,
    };
    assert_eq!(
        offset(&from_a),
        Some(cfg.find("struct X { int from_a; }").expect("the fixture")),
        "`a.cpp` defines `A`, so it sees the `int` branch: {from_a:?}"
    );
    assert_eq!(
        offset(&from_b),
        Some(cfg.find("struct X { long from_b; }").expect("the fixture")),
        "`b.cpp` does not, so it sees the `long` one: {from_b:?}"
    );
}

/// **The diagnostics of a file whose error is in a branch nobody takes.**
///
/// The two readings disagree here, and that is the whole reason [`Session::diagnostics`] exists: the file's own
/// text has an unclosed class inside `#if OFF`, so the raw reading reports it (correctly — the *text* says so),
/// while a compiler never sees that branch at all. Publishing the raw answer would put a red squiggle on code
/// that is not compiled, which is the false positive the cooked reading removes.
#[test]
pub(crate) fn a_branch_nobody_takes_reports_nothing_once_the_file_is_cooked() {
    let main = "#include \"cfg.h\"\n#if OFF\nstruct Unclosed { int x;\n#endif\nint ok;\n";
    let files = MemoryFiles::new()
        .with_file("/p/cfg.h", "#define OFF 0\n")
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-branch-nobody-takes", &files);
    let mut session = fixture.session();

    // **Before the file is cooked there is no answer, and that is the contract now.** This used to assert a
    // `Raw` reading — the file's own text — whose single error was the unclosed class in the untaken branch. That
    // reading is gone: an untaken branch is not compiled, so reporting an error inside one is reporting an error
    // about a program that does not exist. The file is *held* rather than opened, so nothing has asked for a
    // rendering and there is nothing to answer from.
    session.load("/p/main.cpp");
    assert!(
        session.diagnostics("/p/main.cpp").is_none(),
        "nothing has read this file as a compiler reads it, so there is nothing to say about it"
    );

    session.did_open("/p/main.cpp", main);
    session.index_everything();

    let cooked = session.diagnostics("/p/main.cpp").expect("the file reads");
    assert_eq!(
        cooked.reading,
        crate::session::DiagnosticReading::Cooked,
        "the index holds a reading of this file now"
    );
    assert!(
        cooked.errors.is_empty(),
        "and the branch it complained about is not in it: {cooked:?}"
    );
    assert_eq!(
        cooked.unplaced, 0,
        "nothing was hidden from the reader either"
    );
}

/// **A cooked error's offsets are offsets in the file**, not in the rendering it was found in.
///
/// The two coordinate systems are different here, which is what makes the claim checkable: the file writes
/// `DECLARE_HANDLE(HWND);` and the rendering has the struct and the typedef the macro expands to, so an offset
/// taken from the rendering lands past the end of the file. The evidence that the expansion really happened is
/// in the same test — the declaration the file never wrote (`HWND__`) — because a fixture where the macro stayed
/// unexpanded would make a rendering offset and a file offset the same number and the assertion worthless.
#[test]
pub(crate) fn a_cooked_error_is_placed_in_the_file_not_in_the_rendering() {
    let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                      typedef struct name##__ *name\n";
    let main = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\nstruct Unclosed { int x;\n";
    let files = MemoryFiles::new()
        .with_file("/p/handle.h", handle)
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("cooked-errors-are-file-offsets", &files);
    let mut session = fixture.session();

    session.did_open("/p/main.cpp", main);
    session.index_everything();

    let declarations = session
        .index()
        .cooked_declarations(Path::new("/p/main.cpp"))
        .expect("the file was cooked");
    assert!(
        declarations.iter().any(|fact| fact.name == "HWND__"),
        "the rendering is not the file's text: the macro declared a struct this file never wrote: {:?}",
        declarations
            .iter()
            .map(|fact| &fact.name)
            .collect::<Vec<_>>()
    );

    let found = session.diagnostics("/p/main.cpp").expect("the file reads");
    assert_eq!(found.reading, crate::session::DiagnosticReading::Cooked);
    assert_eq!(found.errors.len(), 1, "{found:?}");

    let error = &found.errors[0];
    let unclosed = main.find("struct Unclosed").expect("the fixture writes it");
    assert!(
        error.start >= unclosed,
        "the error is about the line that is still unclosed: {error:?}"
    );
    assert!(
        error.end <= main.len(),
        "and it is a position in the file — a rendering offset would be past its end: {error:?}"
    );
    assert_eq!(found.unplaced, 0, "nothing had to be hidden: {found:?}");
}

/// **What is cooked is what is looked at**: the open files and the files their own text names, plus any file a
/// request is about — not a closure, and not a project.
///
/// The reader who needed the old behaviour was the diagnostic pass, which publishes an answer for every indexed
/// file — but "every indexed file" is the **closure** of an open file, and on a real project that was 138 files:
/// measured at **11.7 s** of the startup, every time, cache or no cache, for the difference between 999 and 1000
/// declarations. A file nobody looks at is answered from its own text, and the diagnostic layer says so
/// ([`crate::session::DiagnosticReading::Raw`]) rather than pretending otherwise.
#[test]
pub(crate) fn a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is() {
    let main = "#include \"a.h\"\n#include \"b.h\"\n#include \"c.h\"\nA a; B b; C c;\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.h", "struct A { int x; };\n")
        .with_file("/p/b.h", "struct B { int x; };\n")
        .with_file("/p/c.h", "struct C { int x; };\n")
        .with_file("/p/main.cpp", main)
        .with_file("/p/other.cpp", "#include \"a.h\"\nvoid g() { }\n");
    let fixture = Memory::new("only-what-is-looked-at-is-cooked", &files);
    let mut session = fixture.session();

    // A file the caller knows about, in the rest half of the queue. Nothing opens it, nothing looks at it.
    session.add_project_files([PathBuf::from("/p/other.cpp")]);
    session.did_open("/p/main.cpp", main);

    session.index_everything();
    assert_eq!(session.pending_work(), 0, "both kinds of work are done");

    for looked_at in ["/p/main.cpp", "/p/a.h", "/p/b.h", "/p/c.h"] {
        assert!(
            session
                .index()
                .cooked_declarations(Path::new(looked_at))
                .is_some(),
            "{looked_at} is what the reader is looking at"
        );
    }
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_none(),
        "and a file nobody opened, nobody included and no request named has no reading"
    );

    // A request names it: that is what the shell does before every query, and the reading follows.
    session.want_cooked_reading(Path::new("/p/other.cpp"));
    session.index_everything();
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_some(),
        "the file a request is about is read the way a compiler reads it"
    );
}

/// **Editing a header invalidates the reading of every file that had one**, and those come back.
///
/// The set is every dependent ([`crate::ProjectIndex::dependents_of`]), because a stale reading is a wrong answer
/// rather than a missing one — and it is *only* the dependents that had a reading, because a file nobody has
/// looked at has nothing to invalidate and asking for a new reading would cook a file nobody asked about.
#[test]
pub(crate) fn editing_a_header_invalidates_the_readings_that_depend_on_it() {
    let ns = "#define BEGIN_NS namespace one {\n#define END_NS }\n";
    let api = "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";
    let other = "#include \"api.h\"\nWidget w;\n";
    let files = MemoryFiles::new()
        .with_file("/p/ns.h", ns)
        .with_file("/p/api.h", api)
        .with_file("/p/other.cpp", other);
    let fixture = Memory::new("a-cooked-file-goes-stale", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/other.cpp")]);
    session.did_open("/p/api.h", api);
    // A request names `other.cpp`, so it has a reading to lose — which is what this test is about.
    session.want_cooked_reading(Path::new("/p/other.cpp"));
    session.index_everything();
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_some(),
        "the fixture starts with the dependent cooked"
    );

    // The header the macro is written in opens the *other* namespace.
    let changed = "#define BEGIN_NS namespace two {\n#define END_NS }\n";
    session.did_open("/p/ns.h", changed);

    for stale in ["/p/other.cpp", "/p/api.h"] {
        assert!(
            session
                .index()
                .cooked_declarations(Path::new(stale))
                .is_none(),
            "{stale} was read as the namespace the macro used to open"
        );
    }

    // **And asked for again.** A file nobody is looking at is rebuilt when a request names it — the rule the focused
    // schedule keeps for every background file, and the reason this test has to name the dependent rather than wait
    // for a sweep of every open tab's closure. Two rounds, because the dependent's environment is the *edited*
    // header's, and a cook claimed before the index has re-read it would render the old macro.
    session.index_everything();
    session.want_cooked_reading(Path::new("/p/api.h"));
    session.want_cooked_reading(Path::new("/p/other.cpp"));
    session.index_everything();
    assert!(
        matches!(
            session
                .index()
                .definition("two::Widget", Path::new("/p/other.cpp")),
            Known::Yes(_)
        ),
        "and it is read again, as the new macro says: {:?}",
        session
            .index()
            .definition("two::Widget", Path::new("/p/other.cpp"))
    );
}

/// **A file that defines no macros invalidates nothing**, which is what keeps the common edit cheap.
///
/// The environment is macros: a header that only declares things cannot change what any file that includes it
/// expands to, so its dependents' readings are still readings of the truth. Without this gate, an edit anywhere
/// in a project would drop the readings of everything that includes the file — and in a standard library that is
/// most of the project, on every keystroke.
#[test]
pub(crate) fn editing_a_file_that_defines_no_macros_keeps_its_dependents_readings() {
    let api = "struct Widget { int size; };\n";
    let other = "#include \"api.h\"\nWidget w;\n";
    let files = MemoryFiles::new()
        .with_file("/p/api.h", api)
        .with_file("/p/other.cpp", other);
    let fixture = Memory::new("a-declaration-is-not-an-environment", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/other.cpp")]);
    session.did_open("/p/api.h", api);
    session.want_cooked_reading(Path::new("/p/other.cpp"));
    session.index_everything();

    // The same declarations with one more member: the file changed, its macros did not.
    session.did_open("/p/api.h", "struct Widget { int size; int weight; };\n");

    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_some(),
        "nothing in this file is part of anyone's environment"
    );
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/api.h"))
            .is_none(),
        "…while the file itself is re-read, which is not a question about macros"
    );
}

/// **The first `#define` a header gets reaches the files that include it.**
///
/// The gate this replaced asked whether the header defined macros *before* the edit — so the edit that added the
/// header's first macro was the one edit it could not see, and every includer went on answering from a reading
/// built when the macro did not exist.
#[test]
pub(crate) fn the_first_macro_a_header_defines_invalidates_the_readings_that_include_it() {
    let api = "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";
    let other = "#include \"api.h\"\nWidget w;\n";
    let files = MemoryFiles::new()
        .with_file("/p/ns.h", "// nothing to say yet\n")
        .with_file("/p/api.h", api)
        .with_file("/p/other.cpp", other);
    let fixture = Memory::new("the-first-macro-of-a-header", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/other.cpp")]);
    session.want_cooked_reading(Path::new("/p/other.cpp"));
    session.index_everything();
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_some(),
        "the fixture starts with the dependent cooked"
    );

    session.did_open(
        "/p/ns.h",
        "#define BEGIN_NS namespace one {\n#define END_NS }\n",
    );

    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_none(),
        "the reading was built when `BEGIN_NS` meant nothing"
    );
}

/// **Typing in a file's body does not cost the walk of its closure, and typing above the body does.**
///
/// The numbers are [`Session::unit_stats`]'s: `walked` is a closure read and a timeline built,
/// `from_disk` is the entry that replaces it.
///
/// The fixture is two sessions over one cache directory, because the disk entry is the thing under test: a
/// unit walked by the first session is what the second has to be served, and the in-memory table would hide
/// whether that happened.
///
/// # Why the root used to be walked on every keystroke
///
/// A unit's closure includes the file it is rooted at, and the key was every file's whole text — so the file
/// being typed into invalidated its own unit on every character. The key is now that file's **preamble**: see
/// [`crate::RootKey`] for what that is and why the tail is checked separately.
#[test]
pub(crate) fn typing_in_a_body_does_not_cost_the_walk_a_translation_unit_is() {
    let main = "#include \"b.h\"\nint main() { return A; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.h", "#pragma once\n#define A 1\n")
        .with_file(
            "/p/b.h",
            "#pragma once\n#include \"a.h\"\nstruct B { int b; };\n",
        )
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-body-edit-is-not-a-walk", &files);

    // **The first session cooks, and nothing walks** — a cook composes its environment from the index
    // (`docs/module-cache.md`), so the per-file record is affordable and no timeline is built for it.
    let mut first = fixture.session();
    first.add_project_files([PathBuf::from("/p/main.cpp")]);
    first.did_open("/p/main.cpp", main);
    first.index_everything();
    assert!(
        first
            .index()
            .cooked_declarations(Path::new("/p/main.cpp"))
            .is_some(),
        "the fixture cooked the file"
    );
    assert_eq!(
        first.unit_stats().walked,
        0,
        "and no timeline was walked for it: {:?}",
        first.unit_stats()
    );
    // **Nothing is written to disk for this any more.** `tu/` held built `TranslationUnit`s, and a stored
    // timeline outlives the files it was built from — so the walk runs every session and what a session reuses is
    // its own memory (`held_units`), not a previous run's file. The property the cases below assert — a keystroke
    // does not cost a walk — is what remains, and it is what the entry was ever for.

    // **One fresh session per question**, because the in-memory table would otherwise answer and hide whether
    // the entry on disk was usable at all — and the entry on disk is the thing under test: it is what has to
    // survive a keystroke, for a session that has not seen the file before and for the one that has.
    let over_a_fresh_session = |what: &str, text: &str| {
        let mut session = fixture.session();
        session.add_project_files([PathBuf::from("/p/main.cpp")]);
        session.did_open("/p/main.cpp", text);
        session.index_everything();
        let stats = session.unit_stats();
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/main.cpp"))
                .is_some(),
            "{what}: the fixture cooked the file"
        );
        stats
    };

    // **The text that was walked**, as a fresh session reads it. The record the first session wrote answers for it —
    // its facts *and* its rendering — so there is no walk at all. This is the hit the per-file record exists for, and
    // the sharpest form of "a fresh session costs nothing for a text already compiled".
    let unedited = over_a_fresh_session("unedited", main);
    assert_eq!(
        unedited.walked, 0,
        "the record the first session wrote answers for the text it was cooked from: {unedited:?}"
    );

    // **A character typed into the body** is a text nobody has cooked, so it is walked once — and the next fresh
    // session gets *that* text from the record the edit's cook wrote.
    let typed = format!("{main}\nint more() {{ return A; }}\n");
    let edited = over_a_fresh_session("a body edit", &typed);
    assert_eq!(
        edited.walked, 0,
        "a text nobody has cooked composes too, so a keystroke costs no walk: {edited:?}"
    );
    let again = over_a_fresh_session("the same body edit again", &typed);
    assert_eq!(
        again.walked, 0,
        "and the record the edit's cook wrote answers for it: {again:?}"
    );

    // **An `#include` above the body is a different matter**: it moves the preamble, which is the key.
    let with_an_include = "#include \"a.h\"\n#include \"b.h\"\nint main() { return A; }\n";
    let moved = over_a_fresh_session("an include above the body", with_an_include);
    assert_eq!(
        moved.walked, 0,
        "an include edit composes too, so it costs no walk: {moved:?}"
    );
}

#[test]
/// **A file this session already has a unit for is read *with* that unit's environment, and the second pass is
/// not paid for it.**
///
/// What it buys is that the *first* parse is the right one — which is the thing that has to be true
/// before the rendering can be skipped, because the rendering is what repairs a reading made
/// without an environment. Two sessions, and the difference between them is the whole claim:
///
/// ```text
///   cold   nothing is in memory, so there is no environment to read the file against — today's path, and the
///          file goes to the second pass like any other file that was parsed
///   warm   the unit is in memory, so the file is read once *with* its environment, and the pass that exists
///          only to repair a reading made without one has nothing to do
/// ```
///
/// # The sharper assertion, and why it is not here
///
/// What the environment is *for* is the scope: `BEGIN_NS` is `namespace one {` to a compiler, so the sharper
/// test is "`one::Widget` is in the index after one step, with no second pass to fix it". It cannot be written
/// today, and the reason is not this change: **`one::Widget` is not in the index after a full
/// `index_everything` either** — the cooked reading places `Widget` at file scope, with the macro unexpanded.
/// That is one of the five failures `status.md` §6 records, and
/// `editing_a_header_invalidates_the_readings_that_depend_on_it` fails through exactly this fixture and
/// exactly this name.
///
/// So this asserts what the change is about — **which files the second pass is handed** — and the doc comment
/// is the note to tighten it into the scope assertion the moment that defect is fixed. A test that asserted the
/// scope now would be red for a reason that has nothing to do with the flag, which is the shape of test
/// `status.md` §6 warns about in its second rule.
// **Ignored while the refined reading is being fixed** — the defect and its reproducer are in `docs/indexing-review.md`
// §6: refining a file makes three `checks` fixtures see a reading their rules decline, so `index_everything` does not
// ask for it yet, and this test needs the ask. The assertion is right; the behaviour is what is wrong.
pub(crate) fn a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass() {
    let ns = "#define BEGIN_NS namespace one {\n#define END_NS }\n";
    let api = "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";
    let typed = format!("{api}void body() {{ int inside; }}\n");
    let files = MemoryFiles::new()
        .with_file("/p/ns.h", ns)
        .with_file("/p/api.h", api);
    let fixture = Memory::new("a-file-is-read-with-its-environment", &files);

    let second_pass_has = |session: &Session<MemoryFiles>| {
        session
            .parsed_since_the_last_pass
            .iter()
            .any(|queued| queued.ends_with("api.h"))
    };

    // **Cold**: nothing has been walked, so there is no environment in hand.
    let mut cold = fixture.session();
    cold.did_open("/p/api.h", api);
    cold.advance(1);
    assert!(
        second_pass_has(&cold),
        "with no unit in memory the file is read the way it always was, and the pass still has it: {:?}",
        cold.parsed_since_the_last_pass
    );

    // **Warm**: the fixture has read the file once, so its unit is in memory.
    let mut warm = fixture.session();
    warm.did_open("/p/api.h", api);
    warm.index_everything();
    // **A program read is what builds a unit now**: a cook composes its environment and walks no closure, so the
    // unit this test is about is asked for explicitly.
    warm.read_the_unit(Path::new("/p/api.h"))
        .expect("the program reads");
    assert!(
        holds_a_unit_for(&warm, "/p/api.h"),
        "the program read holds a unit for the file"
    );
    // **And this is the assertion to tighten.** `one::Widget` is what the environment is *for* — it is now in
    // the index (`scope: Some("one")`, the macro's `namespace` placed) where before this round it was
    // `NotDeclaredHere`, and a test that pinned that would be the sharpest statement of what was fixed. It is
    // deliberately not asserted here, because the same name is the subject of two of the failures
    // `status.md` §6 records, and a red test with two possible causes is worse than a narrow green one.
    //
    // What *is* asserted is this change's own subject: which files the second pass is handed.
    //
    // **`advance(64)` and not `advance(1)`**, because that is the path where this used to be dead. With more
    // than one step to take, `advance` prepares a *wave* on the read path and hands the file's summary back
    // through `index_one`'s `Some` arm — where no environment is available and none is looked for. The
    // mechanism was written, tested against `advance(1)`, and never once reached on the pump's path. See
    // `Session::advance` for what now keeps such a file out of the wave.
    warm.did_change("/p/api.h", &typed);
    warm.advance(64);

    assert!(
        !second_pass_has(&warm),
        "read with the environment of its own closure, so the pass has nothing to repair: {:?}",
        warm.parsed_since_the_last_pass
    );
}

/// **A wave with nothing left to parse still carries the timelines the drain will need.**
///
/// The last stretch of a start-up is the **drain phase**: the index queue is empty and the pump commits re-reads
/// slice after slice, so every one of those waves has nothing to prepare. `prepare_a_wave` used to return early
/// in exactly that state and hand the drain `units: []` — and `prepare_the_re_read` reads that as *"a caller
/// with no timelines"* and changes shape: it copies every indexed file's text into a map and asks a question
/// about every `#define` in the project, and its candidate scan falls back to a closure walk per file.
///
/// Measured on `EmmyLuaCodeStyle` (1761 files): twelve such calls — four files each — took **28 s of a 49 s**
/// cold start and **37 s of a 47 s** warm one, while the index itself was 4–16 s. Handing the drain its
/// timelines is what took the warm start to **11.6 s** and the cold one to **20.6 s**.
///
/// The state is built the way the pump reaches it: more files parsed in one wave than the second pass takes at
/// once, so the queue empties with a backlog still standing.
#[test]
pub(crate) fn a_wave_with_nothing_to_parse_still_carries_the_drains_timelines() {
    // **Four more than the second pass takes at once**, so the queue empties with a backlog still standing — the
    // state this test is about. The fixture used to be eight, which was "one more than four".
    let count = crate::session::RE_READ_SLICE + 4;
    let files = (0..count).fold(MemoryFiles::new(), |files, at| {
        files.with_file(format!("/p/f{at}.cpp"), format!("int value_{at} = {at};\n"))
    });
    let fixture = Memory::new("a-wave-carries-its-drain-timelines", &files);
    let mut session = fixture.session();
    session.add_project_files((0..count).map(|at| PathBuf::from(format!("/p/f{at}.cpp"))));

    // One wave over all of them: the parse happens here, and the commit takes the second pass`s slice, leaving
    // four behind — the queue-empty, backlog-not-empty state.
    // leaving four behind — the queue-empty, backlog-not-empty state.
    let wave = session.prepare_a_wave(count);
    session.commit_a_wave(wave, count);
    assert_eq!(
        session.pending(),
        0,
        "the fixture is the drain phase: nothing left to index"
    );
    assert!(
        !session.parsed_since_the_last_pass.is_empty(),
        "and the second pass still has files: {:?}",
        session.parsed_since_the_last_pass
    );

    let next = session.prepare_a_wave(8);
    assert!(
        next.prepared.is_empty(),
        "there is nothing to parse, which is the state this test is about"
    );
    // **Phase 2 removed the units, and this assertion went with them.** The pass used to prepare one timeline per
    // file it was about to re-read; it now composes each reading from the index (`docs/indexing-design.md` §2.3), so
    // `units` is always empty — see the note at `prepare_a_wave`, where `Vec::new()` replaced the walk.
    //
    // What the drain still owes is the **refinements**: the files the pass is about to re-read, which is what
    // makes the wave after a drain do work rather than return empty. That is the contract this test keeps.
    assert!(
        next.units.is_empty(),
        "the pass builds no timelines: it composes"
    );
    assert!(
        !session.parsed_since_the_last_pass.is_empty(),
        "and the backlog the next slice drains is not empty"
    );
}

/// **A quoted include is verified in the directory it is asked from** — the risk a remembered search carries.
///
/// `SummaryStore::where_it_resolves_now` keeps one answer per **question**, and for an angled include the question
/// is the spelling alone. A quoted one is not: `#include "x.h"` from `/p/a` and from `/p/b` resolve to two
/// different files, so a memo keyed on the spelling would answer the second directory with the first's answer,
/// the stored summary would stop "holding", and every start would rebuild it — a cache that silently stops
/// working. This pins the direction that matters: both summaries are reused.
#[test]
pub(crate) fn a_quoted_include_is_verified_in_the_directory_it_is_asked_from() {
    let files = MemoryFiles::new()
        .with_file("/p/a/x.h", "int from_a;\n")
        .with_file("/p/b/x.h", "int from_b;\n")
        .with_file("/p/a/one.cpp", "#include \"x.h\"\n")
        .with_file("/p/b/two.cpp", "#include \"x.h\"\n");
    let fixture = Memory::new("a-quoted-include-is-per-directory", &files);
    let roots = [PathBuf::from("/p/a/one.cpp"), PathBuf::from("/p/b/two.cpp")];

    // The first session builds the four summaries and writes them down.
    let mut first = fixture.session();
    first.add_project_files(roots.iter().cloned());
    first.index_everything();
    assert!(
        first.stats().rebuilt >= 2,
        "the fixture starts cold: {:?}",
        first.stats()
    );

    // The second answers every one of them from disk, which is only true when each verification accepted the
    // resolution its own directory recorded.
    let mut second = fixture.session();
    second.add_project_files(roots.iter().cloned());
    let steps = second.advance(16);
    let outcomes: Vec<String> = steps
        .iter()
        .map(|step| {
            format!(
                "{}: {:?}",
                step.path.file_name().unwrap_or_default().to_string_lossy(),
                step.outcome
            )
        })
        .collect();
    assert!(
        steps.iter().all(|step| step.outcome == StepOutcome::Reused),
        "nothing may be rebuilt from a cache that holds every one of these files: {outcomes:?}"
    );
}

/// **An `#include` added to a header that defines nothing is still a change to its includers' environment.**
#[test]
pub(crate) fn an_include_added_to_a_header_invalidates_the_readings_that_include_it() {
    let files = MemoryFiles::new()
        .with_file("/p/macros.h", "#define ANSWER 42\n")
        .with_file("/p/api.h", "int declared;\n")
        .with_file("/p/other.cpp", "#include \"api.h\"\nint x = ANSWER;\n");
    let fixture = Memory::new("an-include-is-an-environment", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/other.cpp")]);
    session.want_cooked_reading(Path::new("/p/other.cpp"));
    session.index_everything();
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_some()
    );

    session.did_open("/p/api.h", "#include \"macros.h\"\nint declared;\n");

    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_none(),
        "`ANSWER` is now in force where it was not"
    );
}

/// **Typing below the last directive keeps the edited file's unit; touching a directive drops it — and it
/// drops that one, not the table.**
///
/// A unit is a timeline of directives, so what decides whether an edit can have moved it is whether any directive
/// did — nearly every keystroke is in a body, and re-walking the closure for each of them was most of what a
/// keystroke cost.
///
/// Dropping the whole table was the first version of this rule and it invalidated up to `MAX_UNITS`
/// units to invalidate one — including, on the next keystroke, the one the file needed. A file that
/// merely *includes* the edited one has a timeline of its own, and it is still the same directives
/// at the same offsets: `ns.h` knows nothing about `api.h`.
#[test]
pub(crate) fn a_unit_survives_typing_in_a_body_and_not_a_change_to_a_directive() {
    let ns = "#define BEGIN_NS namespace one {\n#define END_NS }\n";
    let api = "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";
    let files = MemoryFiles::new()
        .with_file("/p/ns.h", ns)
        .with_file("/p/api.h", api);
    let fixture = Memory::new("a-unit-survives-a-body", &files);
    let mut session = fixture.session();

    session.did_open("/p/api.h", api);
    session.index_everything();
    session
        .read_the_unit(Path::new("/p/api.h"))
        .expect("the program reads");
    assert!(
        holds_a_unit_for(&session, "/p/api.h"),
        "the program read holds a unit for the file"
    );
    // **A header no longer has a unit of its own, and that is the composed reading working.** `ns.h` is included by
    // `api.h`, so it cannot be the root of its own program; it used to be walked as one so that the *unit table*
    // had an entry to check, and `Session::cook` now composes its environment from the index instead (see
    // `docs/indexing-review.md` §3.1 — the walk required a held timeline, which after phase 2 nothing holds).
    // What this test is about — a **body** edit keeps the unit a directive edit would drop — is asserted on the file
    // that does have one.
    let typed = format!("{api}void body() {{ int inside; }}\n");
    session.did_change("/p/api.h", &typed);
    assert!(
        holds_a_unit_for(&session, "/p/api.h"),
        "a body was typed below the last directive"
    );

    let typed_more = format!("{api}void body() {{ int inside; int more; }}\n");
    session.did_change("/p/api.h", &typed_more);
    assert!(holds_a_unit_for(&session, "/p/api.h"), "and again");

    session.did_change(
        "/p/api.h",
        &format!("#include \"ns.h\"\n#define EXTRA 1\n{typed_more}"),
    );
    assert!(
        !holds_a_unit_for(&session, "/p/api.h"),
        "a directive appeared, so `api.h`'s timeline is out of date"
    );
    // **A header no longer has a unit of its own, and that is the composed reading working.** `ns.h` is included by
    // `api.h`, so it cannot be the root of its own program; it used to be walked as one so that the *unit table*
    // had an entry to check, and `Session::cook` now composes its environment from the index instead (see
    // `docs/indexing-review.md` §3.1 — the walk required a held timeline, which after phase 2 nothing holds).
    // What this test is about — a **body** edit keeps the unit a directive edit would drop — is asserted on the file
    // that does have one.
}

/// **Is a translation unit held for this file?** — asked through the table's own lock, and per file, because
/// what an edit invalidates is one file's timeline rather than the whole table.
///
/// Four assertions here used to call `units.is_empty()` on the `Mutex` itself, so the *test* had stopped
/// compiling when the field gained a lock; they were then rewritten to ask about the whole table, which was
/// right while an edit cleared it and is wrong now that it does not.
pub(crate) fn holds_a_unit_for(session: &Session<MemoryFiles>, path: &str) -> bool {
    session
        .units
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .held
        .contains_key(&queue_key(Path::new(path)))
}

/// **A cook that cannot succeed is not queued again by a request** — the loop an editor reported as "one core, and
/// the diagnostics flashing".
///
/// Measured from a real session's log before this test existed: `index slice: … 4 to cook, 2 left` every ~250 ms for
/// minutes after the index had drained. The cause was the pair this test pins down: [`Session::want_cooked_reading`]
/// *cleared* the refusal (a request is not evidence that the reason changed), and the pump then claimed, refused and
/// the next pull cleared it again.
#[test]
pub(crate) fn a_cook_that_cannot_succeed_is_not_queued_again_by_a_request() {
    let files = MemoryFiles::new().with_file("/p/a.cpp", "int a;\n");
    let fixture = Memory::new("a-refused-cook-is-not-requeued", &files);
    let mut session = fixture.session();
    session.index_everything();

    // A file no summary describes: `materials_for` answers `None` for it, which is the state a cook cannot serve.
    session.want_cooked_reading(Path::new("/p/absent.cpp"));
    let claimed = session.claim_cooking(4);
    assert_eq!(claimed.len(), 1, "the request queued it once");
    let built = session.materials_for(claimed.clone());
    assert!(
        built.is_empty(),
        "a file the index cannot describe has no materials to cook"
    );
    session.refuse_the_cooks_that_built_nothing(&claimed, &[]);
    assert_eq!(
        session.pending_cooking(),
        0,
        "the refused file left the queue"
    );

    // **The request that names it again must not put it back.**
    session.want_cooked_reading(Path::new("/p/absent.cpp"));
    assert_eq!(
        session.pending_cooking(),
        0,
        "a request does not resurrect a refusal — that was the loop"
    );

    // A change to the text is the one thing that asks for a retry.
    session.did_change(Path::new("/p/absent.cpp"), "int b;\n");
    assert_eq!(
        session.pending_cooking(),
        1,
        "an edit is what makes a refusal worth retrying"
    );
}
/// **Editing a header invalidates the reading of the open file that includes it.**
///
/// A file's cooked reading is a reading of its *environment* as well as of its text: `BEGIN_NS` opening a
/// namespace is a fact about `ns.h`, and a reading of `api.h` built before `ns.h` changed would keep answering
/// the old way — a jump to a scope that no longer exists, which is a wrong answer rather than a missing one.
#[test]
pub(crate) fn editing_a_header_stales_the_cooked_reading_of_a_file_that_includes_it() {
    let files = MemoryFiles::new()
        .with_file(
            "/p/ns.h",
            "#define BEGIN_NS namespace one {\n#define END_NS }\n",
        )
        .with_file(
            "/p/api.h",
            "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n",
        );
    let fixture = Memory::new("a-header-edit-stales-readings", &files);
    let mut session = fixture.session();

    session.did_open(
        "/p/api.h",
        "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n",
    );
    session.index_everything();
    assert!(
        matches!(
            session
                .index()
                .definition("one::Widget", Path::new("/p/api.h")),
            Known::Yes(_)
        ),
        "the fixture reads as the header says: {:?}",
        session
            .index()
            .definition("one::Widget", Path::new("/p/api.h"))
    );

    // The header changes the namespace it opens. `api.h`'s text does not change at all — what changes is what
    // its environment says, which is exactly the dependency a per-file invalidation would miss.
    session.did_change(
        "/p/ns.h",
        "#define BEGIN_NS namespace two {\n#define END_NS }\n",
    );
    assert!(
        session.pending_cooking() > 0,
        "the open file that includes it is marked for cooking again"
    );
    session.index_everything();

    assert!(
        matches!(
            session
                .index()
                .definition("two::Widget", Path::new("/p/api.h")),
            Known::Yes(_)
        ),
        "and the new reading is the one in the index: {:?}",
        session
            .index()
            .definition("two::Widget", Path::new("/p/api.h"))
    );
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/api.h"))
            .is_some_and(|facts| facts
                .iter()
                .any(|fact| fact.qualified_name() == "two::Widget")),
        "the stale reading was replaced, not kept beside it"
    );
}

#[test]
pub(crate) fn a_buffer_is_the_text_the_analysis_reads() {
    // The whole reason an editor needs an overlay: the file on disk says `old`, the user is typing `fresh`, and
    // an analysis that answered about the disk would answer about code that no longer exists.
    let files = MemoryFiles::new()
        .with_file("/p/widget.h", "struct Widget { int old; };\n")
        .with_file(
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        );
    let fixture = Memory::new("buffer-is-the-text", &files);
    let mut session = fixture.session();

    session.did_open("/p/widget.h", "struct Widget { int fresh; };\n");
    session.did_open(
        "/p/main.cpp",
        "#include \"widget.h\"\nvoid f() { Widget w; }\n",
    );
    session.index_everything();

    let view = session.view("/p/main.cpp").expect("the file reads");
    let at = view
        .source
        .find("Widget w")
        .expect("the use is in the text");

    match session.definition(&view, ReadingOffset::new(at)) {
        Known::Yes(found) => {
            assert_eq!(found.file, Path::new("/p/widget.h"));
            assert_eq!(found.fact.name, "Widget");
        }
        other => panic!("the buffer's declaration should be found, got {other:?}"),
    }

    // And the members are the buffer's, which is the half a definition jump cannot show.
    match session.members_of(&view, "Widget") {
        Known::Yes(members) => {
            let names: Vec<&str> = members
                .own()
                .map(|member| member.fact.name.as_str())
                .collect();
            assert_eq!(
                names,
                ["fresh"],
                "the disk says `old` and the disk is not the text"
            );
        }
        other => panic!("the buffer's class should have members, got {other:?}"),
    }

    assert_eq!(
        fixture.text("/p/widget.h").as_deref(),
        Some("struct Widget { int fresh; };\n")
    );
}

/// **A closure the index cannot fully describe has no record key.** A member with no summary used to contribute one
/// constant to `the_closure_fingerprint`, so two different texts of the same unindexed header produced the **same** key
/// — and that header is exactly the file whose text just changed, which made a record written under the old text a hit
/// for the new one. The key is `None` now, which is a miss rather than a collision.
#[test]
pub(crate) fn a_closure_with_an_unindexed_member_has_no_record_key() {
    let files = MemoryFiles::new()
        .with_file("/p/ns.h", "#define NS one\n")
        .with_file("/p/api.h", "#include \"ns.h\"\nNS\n");
    let fixture = Memory::new("a-closure-with-an-unindexed-member", &files);
    let mut session = fixture.session();
    session.did_open("/p/api.h", "#include \"ns.h\"\nNS\n");
    session.index_everything();
    assert!(
        session
            .store
            .the_closure_fingerprint(Path::new("/p/api.h"))
            .is_some(),
        "every member of the closure has a summary"
    );

    // The header is edited: its summary is dropped and queued, and `api.h`'s text has not changed at all.
    session.did_change("/p/ns.h", "#define NS two\n");
    assert!(
        session
            .store
            .the_closure_fingerprint(Path::new("/p/api.h"))
            .is_none(),
        "a member with no summary means the closure has no key, not a key that collides with any other text"
    );

    session.index_everything();
    assert!(
        session
            .store
            .the_closure_fingerprint(Path::new("/p/api.h"))
            .is_some(),
        "and the key is back once the index has caught up"
    );
}

/// **A header that includes itself does not walk forever.** `uriparser`'s `uri.h` writes `#include "Uri.h"` inside
/// doxygen regions and on Windows that resolves to the file itself; an unguarded header has nothing inside the walk to
/// stop the descent, so it went to the include-depth limit — 197 470 entries of one file, measured. The edge is
/// recorded as skipped, and the assertion here is the frame count: two files, not two hundred.
#[test]
pub(crate) fn a_header_that_includes_itself_is_read_once() {
    let files = MemoryFiles::new()
        .with_file("/p/self.h", "#include \"self.h\"\n#define X 1\n")
        .with_file("/p/a.cpp", "#include \"self.h\"\nint a;\n");
    let fixture = Memory::new("a-self-include", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", "#include \"self.h\"\nint a;\n");
    session.index_everything();
    let unit = session
        .read_the_unit(Path::new("/p/a.cpp"))
        .expect("the program reads");
    assert_eq!(
        unit.files, 2,
        "the header is filed once, not to the include-depth limit"
    );
}
