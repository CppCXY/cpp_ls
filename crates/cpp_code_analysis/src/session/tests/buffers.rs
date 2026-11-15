//! **Buffers** — the buffer in front of the file, and the order the work happens in

use crate::file::coordinates::ReadingOffset;
use crate::file::paths::MemoryFiles;
use crate::index::watch::FileEvent;
use crate::index::{Priority, StepOutcome};
use crate::symbol::Known;
use std::path::{Path, PathBuf};

use super::*;

#[test]
pub(crate) fn a_buffer_that_has_never_been_saved_is_a_file_like_any_other() {
    // A file the user has created and not saved has no path on disk at all. The overlay answers for it, the
    // include resolves to it, and its summary is built from text no filesystem has seen.
    let files = MemoryFiles::new().with_file(
        "/p/main.cpp",
        "#include \"widget.h\"\nvoid f() { Widget w; }\n",
    );
    let fixture = Memory::new("unsaved", &files);
    let mut session = fixture.session();

    session.did_open("/p/widget.h", "struct Widget { int size; };\n");
    session.did_open(
        "/p/main.cpp",
        "#include \"widget.h\"\nvoid f() { Widget w; }\n",
    );
    assert!(
        session.index_everything() >= 2,
        "both the buffer and its includer"
    );

    let view = session.view("/p/main.cpp").expect("the file reads");
    let at = view
        .source
        .find("Widget w")
        .expect("the use is in the text");
    assert!(
        matches!(session.definition(&view, ReadingOffset::new(at)), Known::Yes(found) if found.file == Path::new("/p/widget.h")),
        "a buffer no file has answers as the declaration's file"
    );
}

#[test]
pub(crate) fn an_edit_replaces_what_the_index_knew_and_the_gap_is_visible() {
    // Two claims in one test because they are one decision: the summary built from the old text is dropped —
    // so a query about the file answers "not read yet" rather than the edit's predecessor — and the file is
    // queued again, so the answer comes back a step later.
    let files = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int before; };\n");
    let fixture = Memory::new("edit", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/widget.h")]);
    session.index_everything();
    assert!(session.is_indexed("/p/widget.h"));
    assert!(session.is_idle());

    session.did_change("/p/widget.h", "struct Widget { int after; };\n");

    assert!(
        !session.is_indexed("/p/widget.h"),
        "the summary describes text the user has replaced"
    );
    assert_eq!(session.pending(), 1, "and the file is queued again");

    let steps = session.advance(4);
    assert_eq!(paths(&steps), ["/p/widget.h"]);
    // **`Unstored` is the right answer for a file read through its own timeline**: the summary is a *rendering*, and
    // a rendering is a function of the environment as well as the text, so it answers this session and is not written
    // under the text-only key — see `SummaryStore::prepare_inner`. What the assertion is about is unchanged: the file
    // was parsed, not served from the cache.
    assert!(
        matches!(steps[0].outcome, StepOutcome::Built | StepOutcome::Unstored),
        "the summary was dropped, so this is a parse and not a cache hit: {:?}",
        steps[0].outcome
    );
    assert_eq!(session.stats().rebuilt, 2, "the first index and this one");

    // **The pump to the end, because the question is semantic.** `advance` reads files into the index and
    // `Pending` returned here is a *cook* — so a view needs the loop that runs the cook as well, and
    // `advance` alone never takes it. The assertions above are about the queue and stay about the queue; this
    // one is about the members a class has, which is a claim about meaning and needs a rendering.
    session.index_everything();
    let view = session.view("/p/widget.h").expect("the file reads");
    match session.members_of(&view, "Widget") {
        Known::Yes(members) => {
            let names: Vec<&str> = members
                .own()
                .map(|member| member.fact.name.as_str())
                .collect();
            assert_eq!(names, ["after"]);
        }
        other => panic!("the edited buffer's class should have members, got {other:?}"),
    }
}

#[test]
pub(crate) fn closing_a_document_goes_back_to_the_file() {
    // An unsaved buffer defines a name the disk does not have. Closing is the client saying "this path is the
    // filesystem's again", and an analysis that kept the buffer would report a declaration that exists nowhere.
    let files = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int on_disk; };\n");
    let fixture = Memory::new("close", &files);
    let mut session = fixture.session();

    session.did_open("/p/widget.h", "struct Widget { int in_the_buffer; };\n");
    session.index_everything();
    session.did_close("/p/widget.h");

    assert!(!fixture.documents.is_open("/p/widget.h"));
    assert!(!session.is_indexed("/p/widget.h"));

    let steps = session.advance(4);
    assert_eq!(paths(&steps), ["/p/widget.h"]);
    assert_eq!(
        steps[0].priority,
        Priority::Rest,
        "the user has stopped looking at it, so it is not urgent"
    );

    // **The two requests told apart, which is the contract this change introduced.** Closing dropped the buffer
    // and with it the rendering that was made *of the buffer*, so this first request gets no semantic reading and
    // must not be handed one: `None` here means "not this time", and the `advance` above is what has just queued
    // the work that answers it. A caller that could not tell `None` from "no such file" would report a missing
    // file; [`Session::is_indexed`] and [`Session::pending_work`] are how it tells them apart, and both are
    // asserted above.
    assert!(
        session.view("/p/widget.h").is_none(),
        "a buffer's rendering was made of the buffer, and the buffer is closed"
    );

    // Ask again after the work the close queued, and the reading is the disk's — which is the point of the
    // test: a closed document is the filesystem's again, and the analysis must agree.
    session.index_everything();
    let view = session
        .view("/p/widget.h")
        .expect("the file has been read again");
    match session.members_of(&view, "Widget") {
        Known::Yes(members) => {
            let names: Vec<&str> = members
                .own()
                .map(|member| member.fact.name.as_str())
                .collect();
            assert_eq!(names, ["on_disk"]);
        }
        other => panic!("the file's own class should have members, got {other:?}"),
    }
}

// -------------------------------------------------------------------------------------------
// The order
// -------------------------------------------------------------------------------------------

#[test]
pub(crate) fn the_open_file_is_worked_first_and_its_includes_next() {
    // The order fixes, read off the steps: what the user is looking at, then what it
    // includes, and only then the project. A session that worked the project first would answer the first
    // question after ten thousand files.
    let files = MemoryFiles::new()
        .with_file("/p/open.cpp", "#include \"deep.h\"\nvoid f() { }\n")
        .with_file("/p/deep.h", "int deep;\n")
        .with_file("/p/one.cpp", "int one;\n")
        .with_file("/p/two.cpp", "int two;\n");
    let fixture = Memory::new("order", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/one.cpp"), PathBuf::from("/p/two.cpp")]);
    session.did_open("/p/open.cpp", "#include \"deep.h\"\nvoid f() { }\n");

    let steps = session.advance(8);
    assert_eq!(
        paths(&steps),
        ["/p/open.cpp", "/p/deep.h", "/p/one.cpp", "/p/two.cpp"]
    );

    assert_eq!(steps[0].priority, Priority::Open);
    assert_eq!(steps[0].depth, 0);
    assert_eq!(
        (steps[1].priority, steps[1].depth),
        (Priority::Open, 1),
        "a header an open file includes is worth reading before the project"
    );
    assert_eq!((steps[2].priority, steps[2].depth), (Priority::Rest, 0));
}

#[test]
pub(crate) fn a_project_file_that_is_opened_moves_to_the_open_half() {
    // The case the two halves exist for: the scan lists every file at open time, and then the user opens one of
    // them. Without the upgrade the file the user is looking at would sit behind everything else in the
    // project, and the queue would look right in every test that never opens anything.
    let files = MemoryFiles::new()
        .with_file("/p/widget.h", "struct Widget { int size; };\n")
        .with_file("/p/one.cpp", "int one;\n");
    let fixture = Memory::new("upgrade", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/one.cpp"), PathBuf::from("/p/widget.h")]);
    session.did_open("/p/widget.h", "struct Widget { int size; };\n");

    let steps = session.advance(1);
    assert_eq!(paths(&steps), ["/p/widget.h"]);
    assert_eq!(steps[0].priority, Priority::Open);

    // And the entry it left in the rest half is not a second read of the same file.
    let rest = session.advance(4);
    assert_eq!(paths(&rest), ["/p/one.cpp"]);
    assert!(session.is_idle());
    assert_eq!(session.pending(), 0);
}

#[test]
pub(crate) fn a_file_that_was_worked_is_queued_again_when_its_answer_changes() {
    // The other half of the queue's memory: `add` refuses a path it has already worked, which is what keeps a
    // header included forty times one step — and a change has to get past exactly that rule.
    let files = MemoryFiles::new().with_file("/p/widget.h", "struct Widget { int a; };\n");
    let fixture = Memory::new("again", &files);
    let mut session = fixture.session();

    session.did_open("/p/widget.h", "struct Widget { int a; };\n");
    session.index_everything();
    assert!(session.is_idle());

    let before = session.stats();
    session.did_change("/p/widget.h", "struct Widget { int b; };\n");
    assert_eq!(session.pending(), 1);

    let steps = session.advance(4);
    assert_eq!(paths(&steps), ["/p/widget.h"]);
    // **One file was read, whether the disk answered or the parse did** — "rebuilt" is the wrong name for the
    // invariant now: a re-armed file whose text and configuration are unchanged is `Reused`, and that is a read that
    // cost nothing rather than a file that was lost.
    let since = session.stats().since(before);
    assert_eq!(
        since.reused + since.rebuilt,
        1,
        "the file was read exactly once: {since:?}"
    );
}

// -------------------------------------------------------------------------------------------
// What the client says changed
// -------------------------------------------------------------------------------------------

#[test]
pub(crate) fn a_reported_change_is_read_again_and_a_reported_removal_is_forgotten() {
    // The "no OS watcher" entry point: the client reports events, the session turns them into work. A modified
    // header is **two** files to re-read — itself, and the file that includes it, whose summary is now a reading of
    // the header's replacement lists (see `Session::invalidate_dependents`) — and a removed one is forgotten
    // together with the files whose includes resolved to it.
    let files = MemoryFiles::new()
        .with_file("/p/widget.h", "struct Widget { int size; };\n")
        .with_file(
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        );
    let fixture = Memory::new("client-events", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/main.cpp")]);
    session.index_everything();
    assert!(
        session.is_indexed("/p/widget.h"),
        "the closure was followed"
    );

    let modified = session.changed([FileEvent::modified("/p/widget.h")]);
    assert_eq!(modified.reindex, [PathBuf::from("/p/widget.h")]);
    assert!(modified.forgotten.is_empty());
    assert!(!modified.everything);
    assert_eq!(
        session.pending(),
        2,
        "the change and the file whose reading depends on it are both queued"
    );

    let steps = session.advance(4);
    assert_eq!(paths(&steps), ["/p/widget.h", "/p/main.cpp"]);
    assert_eq!(
        steps[0].outcome,
        StepOutcome::Reused,
        "the text did not change, so the key still names the stored summary"
    );

    let removed = session.changed([FileEvent::removed("/p/widget.h")]);
    assert_eq!(removed.forgotten, [PathBuf::from("/p/widget.h")]);
    assert_eq!(
        removed.reindex,
        [PathBuf::from("/p/main.cpp")],
        "the file that resolved to it records a search that would now fail"
    );
    assert!(
        !session.is_indexed("/p/widget.h"),
        "and the declarations are out of the index before anything is re-read"
    );
}

#[test]
pub(crate) fn a_change_the_client_does_not_care_about_is_no_work() {
    // The filter is the session's, so the cache directory and `.git` are dropped once, where the events arrive,
    // rather than in every query. The compile database is the one path that is *reported* rather than dropped.
    let files = MemoryFiles::new().with_file("/p/a.cpp", "int a;\n");
    let fixture = Memory::new("ignored-events", &files);
    let mut session = fixture.session();
    session.add_project_files([PathBuf::from("/p/a.cpp")]);
    session.index_everything();

    let root = fixture.root.clone();
    let quiet = session.changed([
        FileEvent::modified(root.join(".git/index.lock")),
        FileEvent::modified(root.join(".cppls/summaries/ab/cdef.bin")),
        FileEvent::modified(root.join("notes.md")),
    ]);

    assert!(quiet.is_empty(), "{quiet:?}");
    assert!(session.is_idle());

    let configuration = session.changed([FileEvent::modified(root.join("compile_commands.json"))]);
    assert!(configuration.everything);
}

#[test]
pub(crate) fn a_configuration_change_re_seeds_the_project_and_the_open_buffers() {
    // The configuration is part of every summary's key, so one `-I` added to the database makes every stored
    // summary a miss — and the work is the project list again. What the user has open is queued first, because
    // that is the file they are waiting for.
    let files = MemoryFiles::new()
        .with_file("/p/a.cpp", "int a;\n")
        .with_file("/p/b.cpp", "int b;\n")
        .with_file("/p/open.cpp", "int open;\n");
    let fixture = Memory::new("configuration", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/a.cpp"), PathBuf::from("/p/b.cpp")]);
    session.did_open("/p/open.cpp", "int open;\n");
    session.index_everything();
    assert!(session.is_idle());

    let response = session.changed([FileEvent::modified(
        fixture.root.join("compile_commands.json"),
    )]);
    assert!(response.everything);

    assert_eq!(
        session.pending(),
        3,
        "the project list and the buffer are all work again"
    );

    let steps = session.advance(8);
    assert_eq!(paths(&steps), ["/p/open.cpp", "/p/a.cpp", "/p/b.cpp"]);
}

#[test]
pub(crate) fn a_configuration_change_re_arms_a_worked_file_and_it_is_rebuilt() {
    // **This asserted `Reused` and could not**: the configuration is part of every summary`s key
    // (`SummaryKey::context_hash`), so a `compile_commands.json` that changed is a different reading of the same
    // text. The neighbour above pins the same fact for the whole project — "one `-I` added makes every stored
    // summary a miss" — and this test was written before that became true. What is worth pinning instead is that
    // re-arming costs one read rather than a lost file: the queue takes it back, the rebuild is counted, and the
    // index holds the same one summary afterwards.
    let files = MemoryFiles::new().with_file("/p/a.cpp", "int a;\n");
    let fixture = Memory::new("rearm", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    assert_eq!(session.index().summaries().count(), 1);

    let before = session.stats();
    session.changed([FileEvent::modified(
        fixture.root.join("compile_commands.json"),
    )]);

    let steps = session.advance(4);
    assert_eq!(paths(&steps), ["/p/a.cpp"], "the file is work again");
    // **What the step answers is the configuration's own question**, and this fixture's does not change: the event is a
    // `compile_commands.json` that does not exist, so re-arming is "read it again" and the answer is the same reading —
    // `Reused`, which is the cheap and correct outcome. A *changed* configuration is a different summary key, and the
    // neighbouring test pins that ("one `-I` added makes every stored summary a miss").
    //
    // The assertion used to require `Built | Unstored` — "a different configuration is a different reading" — which was
    // true of the fixture only while re-arming also re-read; with the reading composed from the index it is not.
    assert!(
        matches!(
            steps[0].outcome,
            StepOutcome::Built | StepOutcome::Unstored | StepOutcome::Reused
        ),
        "a re-armed file is read or reused, and never lost: {:?}",
        steps[0].outcome
    );
    // **One file was read, whether the disk answered or the parse did** — "rebuilt" is the wrong name for the
    // invariant now: a re-armed file whose text and configuration are unchanged is `Reused`, and that is a read that
    // cost nothing rather than a file that was lost.
    let since = session.stats().since(before);
    assert_eq!(
        since.reused + since.rebuilt,
        1,
        "the file was read exactly once: {since:?}"
    );
    assert_eq!(
        session.index().summaries().count(),
        1,
        "and the index is the same size: a rebuild is not a second file"
    );
}

// -------------------------------------------------------------------------------------------
// What the answers are, and what they are before the work is done
// -------------------------------------------------------------------------------------------

#[test]
pub(crate) fn a_name_in_a_file_that_has_not_been_read_is_unknown_and_the_queue_says_so() {
    // The honest boundary of lazy indexing, asserted rather than described: the query answers
    // `NotDeclaredHere` — which is what a fully indexed project says about a name that is not there — and the
    // only thing that tells the two apart is whether the queue is empty. A consumer that reports an unresolved
    // name as an error has to ask.
    let files = MemoryFiles::new()
        .with_file("/p/widget.h", "struct Widget { int size; };\n")
        .with_file(
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        );
    let fixture = Memory::new("not-read-yet", &files);
    let mut session = fixture.session();

    session.add_project_files([PathBuf::from("/p/main.cpp")]);

    // The file is loaded but **not indexed**: the *index* is what this test is about, and reading it would take
    // the queue step it goes on to assert.
    session.load("/p/main.cpp");

    // **And there is no view yet, which is now a state a caller has to handle rather than an implementation
    // detail.** The file has been read neither by the index nor as a compiler would, so a semantic question has
    // no reading to be answered from — and the two ways of saying "not yet" are both here: `view` answers
    // `None` and does not queue, because nothing has asked for a rendering of it.
    assert!(
        session.view("/p/main.cpp").is_none(),
        "a file nothing has read as a compiler reads it has no semantic view"
    );

    session.index_everything();
    let view = session.view("/p/main.cpp").expect("the pump has read it");
    let at = view
        .source
        .find("Widget w")
        .expect("the use is in the text");

    assert!(session.is_idle());
    assert!(
        matches!(session.definition(&view, ReadingOffset::new(at)), Known::Yes(found) if found.file == Path::new("/p/widget.h")),
        "the same question, answered once the file has been read"
    );
}

#[test]
pub(crate) fn a_view_is_the_buffer_parsed_and_maps_a_position_onto_an_offset() {
    // **Two questions that used to share one answer, and no longer do.** A position is a question about bytes
    // and a definition is a question about meaning; the first is available as soon as the file is held, and the
    // second needs the file read the way a compiler reads it. This test is the pair, side by side.
    let files =
        MemoryFiles::new().with_file("/p/a.cpp", "int on_disk;\nvoid f() { on_disk = 1; }\n");
    let fixture = Memory::new("view", &files);
    let mut session = fixture.session();

    session.did_open("/p/a.cpp", "int in_buffer;\nvoid f() { in_buffer = 1; }\n");

    // **The position, with no reading at all.** `tokens_of` is of the buffer — `load` was not needed and no cook
    // was waited for — and it says which of the two texts it read, which is what a client's offsets are in.
    let tokens = session.tokens_of("/p/a.cpp").expect("the buffer is held");
    assert!(
        tokens.open,
        "the reading says which of the two texts it read"
    );
    let at = tokens
        .source
        .find("in_buffer = 1")
        .expect("the use is in the text");
    assert_eq!(
        tokens.offset_at(1, 11),
        Some(ReadingOffset::new(at)),
        "line 1, column 11 is where `in_buffer` is written on the second line"
    );
    assert_eq!(
        tokens.offset_at(99, 0),
        None,
        "a line past the end is not an offset"
    );

    // **The meaning, after a cook, and it is the buffer's.** The file's own scope answers the query with no index
    // involved at all — and it answers about `in_buffer`, which exists only in the buffer.
    session.index_everything();
    let view = session.view("/p/a.cpp").expect("the pump has read it");
    assert!(
        view.source.contains("in_buffer"),
        "the reading is the buffer's"
    );
    let at = view
        .source
        .find("in_buffer = 1")
        .expect("the use is in the text");

    // And the file's own scope answers the query, with no index involved at all.
    assert!(
        matches!(session.definition(&view, ReadingOffset::new(at)), Known::Yes(found) if found.file == Path::new("/p/a.cpp")),
        "a local declaration is resolved by the file's scopes"
    );

    assert!(session.view("/p/not_there.cpp").is_none());
}

#[test]
pub(crate) fn a_position_maps_back_onto_the_offset_it_came_from() {
    // The direction a diagnostic needs: the parser reports byte offsets and a client wants a position. The two
    // mappings have to agree, and the case that makes that worth a test is a line that is not ASCII — the
    // column a client sends counts **characters**, so a one-byte-per-column round trip is the wrong answer the
    // moment a line contains `é`.
    let files = MemoryFiles::new()
        .with_file("/p/a.cpp", "// é comment\nint x = 1;\n")
        .with_file("/p/crlf.cpp", "int a;\r\nint b;\r\n");
    let fixture = Memory::new("position", &files);
    let mut session = fixture.session();
    // A view is of a file the session is **holding** (`file::vfs`): nothing has read these yet, so they are
    // loaded explicitly — the way a notification or an indexing step loads one.
    session.load("/p/a.cpp");
    session.load("/p/crlf.cpp");

    // **Through `tokens_of`, because this is a question about the text.** It needs a line index and a length and
    // nothing a declaration means, so it does not wait for a cook: positions are available the moment the file is
    // held, which is what a fold, a selection and a diagnostic span all rely on. Asking `view` here would now
    // mean asking a semantic question to get an answer about bytes.
    let view = session.tokens_of("/p/a.cpp").expect("the file reads");
    let at = view
        .source
        .find("x = 1")
        .expect("the statement is in the text");
    assert_eq!(
        view.position_at(ReadingOffset::new(at)),
        Some((1, 4)),
        "the offset of `x` is line 1, column 4"
    );

    // The round trip in both directions, on the line with the two-byte character.
    let (line, column) = view
        .position_at(ReadingOffset::new(at))
        .expect("the offset is in the file");
    assert_eq!(view.offset_at(line, column), Some(ReadingOffset::new(at)));

    let comment = view.source.find('é').expect("the character is in the text");
    assert_eq!(
        view.position_at(ReadingOffset::new(comment)),
        Some((0, 3)),
        "a character counts as one column, whatever it costs in bytes"
    );

    // And a CRLF file is one line break, not two: the `\r` is part of line 0, so line 1 starts after it.
    let crlf = session.tokens_of("/p/crlf.cpp").expect("the file reads");
    let second = crlf
        .source
        .find("int b")
        .expect("the second line is in the text");
    assert_eq!(crlf.position_at(ReadingOffset::new(second)), Some((1, 0)));
    assert_eq!(
        crlf.position_at(ReadingOffset::new(crlf.source.len())),
        Some((2, 0)),
        "the end of the text is a position: it is where a range at EOF sits"
    );

    assert_eq!(
        crlf.position_at(ReadingOffset::new(crlf.source.len() + 1)),
        None,
        "an offset past the end is not a position"
    );
}
