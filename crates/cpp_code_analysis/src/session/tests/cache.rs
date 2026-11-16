//! **Cache** — what the session keeps: a rendering, a refinement, the sharing file

use crate::file::paths::MemoryFiles;
use crate::symbol::Known;
use std::path::{Path, PathBuf};

use super::*;

/// **A summary that describes a *rendering* is not left in the cache for the next session.**
///
/// `api.h` declares `Live` one way when `FROM_THE_SOURCE` is defined and another when it is not, so the file read
/// **inside `main.cpp`'s program** is not the file read **on its own**. The cache key is the file's text plus the
/// compilation's context, and a rendering is a function of the text *and of the environment at the point the file was
/// included* — so a rendering-derived summary written under that key is a row which says something the next session
/// has no way to ask for, and a session that reads it answers another translation unit's reading.
///
/// Measured before this rule: the same project indexed twice answered **43 238** and **43 483** declarations in one
/// closure (`tests/unit_read_probe.rs`), and 36 name answers in both — a small enough difference to look like noise,
/// which is why it went unexplained for a round.
#[test]
pub(crate) fn a_rendering_is_not_left_in_the_cache_for_the_next_session() {
    let api = "#ifdef FROM_THE_SOURCE\nstruct Live { int a; };\n#else\nstruct Live { long b; };\n#endif\n";
    let main = "#define FROM_THE_SOURCE 1\n#include \"api.h\"\nLive live;\n";
    let files = MemoryFiles::new()
        .with_file("/p/api.h", api)
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-rendering-is-not-cached", &files);

    // **The run that renders**: `main.cpp`'s program defines the macro, so `api.h`'s reading there is the `int`
    // branch and the summary built for it is that rendering.
    {
        let mut session = fixture.session();
        session.did_open("/p/main.cpp", main);
        session.index_everything();
        let reading = session
            .read_a_looked_at_unit()
            .expect("the program is read");
        assert!(reading.files >= 2, "two files: {reading:?}");
        assert!(
            session
                .index()
                .cooked_declarations(Path::new("/p/api.h"))
                .is_some_and(|facts| facts.iter().any(|fact| fact.name == "Live")),
            "the program's reading of the header is in this session"
        );
    }

    // **The next session knows nothing about that program.** `api.h` read on its own takes the `#else` branch, so
    // that is the answer a *query* must get.
    //
    // **What the summary says is a different question, and the answer changed with the design** (§2.1 of
    // `docs/indexing-design.md`): a summary is the **environment-free** reading, so it keeps *both* branches of the
    // `#if` and the **lookup** decides which one the program takes. That is why the assertion below is two assertions:
    // the summary holding `Live` twice is the new contract, and the query answering the `#else` one is the guarantee
    // this test exists for — a summary made from `main.cpp`'s rendering would answer the `int` branch instead.
    let mut session = fixture.session();
    session.add_project_files([std::path::PathBuf::from("/p/api.h")]);
    session.index_everything();
    let summary = session
        .index()
        .summary(Path::new("/p/api.h"))
        .expect("the header is read")
        .clone();
    let live: Vec<_> = summary
        .declarations
        .iter()
        .filter(|fact| fact.name == "Live")
        .collect();
    assert_eq!(
        live.len(),
        2,
        "a summary is the environment-free reading, so both branches are in it: {live:?}"
    );
    let from_the_source = api.find("struct Live { int a; }").expect("the fixture");
    let alone = api.find("struct Live { long b; }").expect("the fixture");
    assert!(
        live.iter()
            .any(|fact| fact.range.start_offset == from_the_source),
        "including the branch `main.cpp`'s program took: {live:?}"
    );
    // **What a query answers, and the one thing it must not answer.** The header alone cannot decide
    // `#ifdef FROM_THE_SOURCE` — nothing in its own text defines the macro — so the index answers `Ambiguous`,
    // which is the honest "there are two of these and I cannot tell which". What it must **not** answer is
    // `main.cpp`'s branch: that is the leak this test exists for, and it is what the old summary-keyed cache did.
    let found = session.index().definition("Live", Path::new("/p/api.h"));
    match found {
        Known::Yes(found) => assert_eq!(
            found.fact.range.start_offset, alone,
            "and a query answers the branch this file has when nothing defines `FROM_THE_SOURCE`"
        ),
        Known::Unknown(crate::UnknownReason::Ambiguous(_)) => {}
        other => {
            panic!("`Live` is declared in the header, and not as `main.cpp` read it: {other:?}")
        }
    }
}
#[test]
/// **A reading the second pass found nothing to repair in is written down, and served to the next session.**
///
/// The pass refines a file against its closure, and for the large majority of a real project the answer is "nothing to
/// repair" — no macro body the file's text mentions, no declaration a conditional splits. That answer is worth keeping:
/// without it every start re-examines every file and, worse, builds the **whole-closure timelines** the drain needs to
/// do the examining. Measured on `EmmyLuaCodeStyle`: 1745 files re-read on every start (~120 s), of which the ones with
/// something to decide are a small minority.
///
/// The entry names the file's **own** closure (`the_closure_fingerprint`), so only a reading of that closure is served;
/// the direction that must never happen — a reading taken inside *another* file's program — is pinned by
/// `a_rendering_is_not_left_in_the_cache_for_the_next_session`.
// **Ignored while the refined reading is being fixed** — the defect and its reproducer are in `docs/indexing-review.md`
// §6: refining a file makes three `checks` fixtures see a reading their rules decline, so `index_everything` does not
// ask for it yet, and this test needs the ask. The assertion is right; the behaviour is what is wrong.
pub(crate) fn a_refinement_is_served_to_the_next_session() {
    // Nothing here needs repairing: one declaration, no macro, no conditional. The pass marks the reading and the next
    // session should answer from that mark, without a timeline.
    let api = "struct Live { int a; };\n";
    let main = "#include \"api.h\"\nLive live;\n";
    let files = MemoryFiles::new()
        .with_file("/p/api.h", api)
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-refinement-is-served", &files);
    let roots = [PathBuf::from("/p/main.cpp")];

    {
        let mut first = fixture.session();
        first.add_project_files(roots.iter().cloned());
        // **The ask**: a batch no longer refines what it read (see the loop in `Session::index_everything`).
        first.want_refinement(Path::new("/p/api.h"));
        first.index_everything();
        assert!(first.is_indexed("/p/api.h"), "the closure was followed");
    }

    let mut second = fixture.session();
    second.add_project_files(roots.iter().cloned());
    second.want_refinement(Path::new("/p/api.h"));
    second.index_everything();
    assert_eq!(
        second.unit_stats().walked,
        0,
        "a reading nothing had to repair needs no timeline on the next start: {:?}",
        second.unit_stats()
    );
    // **What this test is named for.** `re_read_backlog` is legitimately non-zero for a file nobody asked about, so
    // that assertion was about the eager contract rather than about the cache: the subject is that the refinement was
    // **served** rather than rebuilt.
    assert!(
        second.stats().stored_refinements_served >= 1,
        "the refinement came from the cache, not from a walk: {:?}",
        second.stats()
    );
}

/// **A declaration only a macro makes becomes one this session answers for**, once its queue drains.
///
/// `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND` to a compiler and *nothing* to a reader of the file's own
/// text: the declaration is inside the macro's replacement list, and the invocation is not a declaration. So the
/// index has no `HWND__` before the file is cooked and has one after — which is the whole reason the reading
/// exists.
///
/// (The other half of what cooking buys — the *scope* a namespace-opening macro puts a declaration in — is
/// already handled by the raw reading when the indexer is given the closure's macro bodies: `BEGIN_NS` is
/// `namespace ns {` to the parser and a declaration inside it is scoped correctly. That is the two-hop reading of
/// a macro-shaped scope and of a body written in another file, and it is why this test is about a declaration
/// the file does not write at all rather than about one it writes.)
#[test]
pub(crate) fn the_sharing_cache_survives_a_restart() {
    let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; };\n";
    let source = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
    let files = MemoryFiles::new()
        .with_file("/p/handle.h", handle)
        .with_file("/p/api.h", source);
    let fixture = Memory::new("the-sharing-cache-survives-a-restart", &files);

    // **The run that fills it.**
    let written = {
        let mut session = fixture.session();
        // The sharing is off by default — see `Session::set_sharing_contributions` — and this test is about the cache
        // that sharing fills, so it asks for it.
        session.set_sharing_contributions(true);
        session.did_open("/p/api.h", source);
        session.index_everything();
        session
            .save_the_sharing()
            .expect("the cache directory is the fixture's")
    };
    assert!(
        written > 0,
        "a run that walked files must have something to write"
    );

    // **A restart**: the memory is empty, the disk is not.
    // A restart: the store a session opens has its own table, empty.
    let empty = fixture.session();
    assert_eq!(
        empty.walk_contributions_held(),
        0,
        "a new store holds nothing"
    );

    let session = fixture.session();
    let restored = session.load_the_sharing();
    assert_eq!(restored, written, "what was written is what is read back");
    assert_eq!(
        session.walk_contributions_held(),
        restored,
        "and it is in the cache the walk consults, not merely counted"
    );

    // **It is a table in the project's database, not a file beside it.** The old shape was `<cache>/sharing.bin`,
    // written whole and decoded whole on every start, so a truncated one lost the lot. Pinned here because the
    // difference is invisible from the two calls above: both shapes round-trip.
    if let Some(database) = session.store.database() {
        let directory = database.directory().to_path_buf();
        assert!(
            !directory.join("sharing.bin").exists(),
            "the sharing cache is not a file any more: {}",
            directory.display()
        );
    }
}

/// **A rendering is asked for once per text.** A query that misses holds no rendering, and a cache lookup cannot
/// tell "nobody has cooked this" from "the cook was evicted" — asking on every miss is what made the pump cook the
/// same header once per drain. A file whose text moves is askable again (see `buffer_changed`).
#[test]
fn a_rendering_is_asked_for_once_per_text() {
    let source = "#include \"b.h\"\nint main() { return 0; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/b.h", "#define B 1\n")
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("asked-once-per-text", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.index_everything();

    let asks = |session: &crate::session::Session<MemoryFiles>| asks_of(session);

    // **What eviction looks like.** The rendering the index had built is dropped, and the next query cannot answer:
    // it asks, once.
    session.forget_the_renderings();
    assert!(
        session.view(Path::new("/p/a.cpp")).is_none(),
        "nothing has a rendering after the cache was dropped"
    );
    assert_eq!(asks(&session), 1, "the miss asks once");

    // The ask is taken and served, and the cache loses the rendering again.
    session.advance(8);
    session.forget_the_renderings();
    assert!(session.view(Path::new("/p/a.cpp")).is_none());
    assert_eq!(
        asks(&session),
        1,
        "an evicted reading is not asked for a second time by the same text"
    );
}

/// How many renderings a query has asked for, for a session of any provider.
fn asks_of(session: &crate::session::Session<MemoryFiles>) -> usize {
    session
        .renderings_asked
        .lock()
        .map(|asked| asked.len())
        .unwrap_or(0)
}
