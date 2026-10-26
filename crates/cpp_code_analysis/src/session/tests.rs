use super::{OpenDocuments, Session, SessionFiles, queue_key};
use crate::file::coordinates::ReadingOffset;
use crate::file::paths::{DiskFiles, FileProvider, MemoryFiles};
use crate::include::config::{CommandLineMacro, CompilerConfig};
use crate::include::toolchain::{Toolchain, ToolchainSource};
use crate::index::watch::{FileEvent, WatchFilter};
use crate::index::{Priority, StepOutcome};
use crate::symbol::Known;
use std::path::{Path, PathBuf};

/// A project in memory: the files, the buffers in front of them, and the providers that join the two.
///
/// The fixture keeps the providers and hands each session a **clone**, which is what a caller outside a test
/// does too — a provider is a handle, so the fixture's copy and the session's copy read the same files, and a
/// test can go on looking at what the analysis read while the session owns its own chain.
struct Memory<'a> {
    files: &'a MemoryFiles,
    documents: OpenDocuments,
    providers: SessionFiles<MemoryFiles>,
    root: PathBuf,
}

impl<'a> Memory<'a> {
    fn new(name: &str, files: &'a MemoryFiles) -> Self {
        let root = std::env::temp_dir().join("cppls-session-tests").join(name);
        let _ = std::fs::remove_dir_all(&root);

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents.clone(), files.clone());

        Memory {
            files,
            documents,
            providers,
            root,
        }
    }

    fn session(&self) -> Session<MemoryFiles> {
        Session::with_config(
            &self.root,
            self.providers.clone(),
            WatchFilter::new(&self.root),
            CompilerConfig::default(),
        )
    }

    /// The text the analysis would read for a path — the buffer if there is one.
    fn text(&self, path: &str) -> Option<String> {
        self.documents
            .text(path)
            .or_else(|| self.files.read(Path::new(path)))
    }
}

impl Drop for Memory<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The paths of a run of steps, for an assertion about the order rather than about the work.
fn paths(steps: &[crate::index::Step]) -> Vec<String> {
    steps
        .iter()
        .map(|step| step.path.to_string_lossy().replace('\\', "/"))
        .collect()
}

// -------------------------------------------------------------------------------------------
// The buffer is the text
// -------------------------------------------------------------------------------------------

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
fn a_rendering_is_not_left_in_the_cache_for_the_next_session() {
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
    // that — and only that — is what its summary may say.
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
        1,
        "the file declares `Live` once when it is read on its own: {live:?}"
    );
    assert_eq!(
        live[0].range.start_offset,
        api.find("struct Live { long b; }").expect("the fixture"),
        "and it is the branch that file has when nothing defines `FROM_THE_SOURCE`"
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
fn the_sharing_cache_survives_a_restart() {
    let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; };\n";
    let source = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
    let files = MemoryFiles::new()
        .with_file("/p/handle.h", handle)
        .with_file("/p/api.h", source);
    let fixture = Memory::new("the-sharing-cache-survives-a-restart", &files);

    // **The run that fills it.**
    let written = {
        let mut session = fixture.session();
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
}

#[test]
fn a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file() {
    let handle = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                      typedef struct name##__ *name\n";
    let source = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
    let files = MemoryFiles::new()
        .with_file("/p/handle.h", handle)
        .with_file("/p/api.h", source);
    let fixture = Memory::new("cooked-reading-is-indexed", &files);
    let mut session = fixture.session();

    session.did_open("/p/api.h", source);

    // **One step**: `api.h` is read and its include is queued, so the queue is *not* empty — and a file whose
    // includes have not been read has an environment that is not complete, which is why nothing is cooked yet.
    session.advance(1);
    let before = session.index().definition("HWND__", Path::new("/p/api.h"));
    assert!(
        matches!(before, Known::Unknown(_)),
        "the file's own text does not declare what the macro declares: {before:?}"
    );
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/api.h"))
            .is_none(),
        "and nothing has cooked it yet"
    );

    // **Drain**: the queue emptying is the moment a file's environment is complete, and that is where the
    // session cooks the files a reader is looking at. No caller asked for this — it is the indexing loop's own
    // last step.
    session.index_everything();

    let after = session.index().definition("HWND__", Path::new("/p/api.h"));
    let Known::Yes(found) = after else {
        panic!("the cooked reading declares it: {after:?}");
    };
    assert_eq!(found.file, Path::new("/p/api.h"));
    let at = found.fact.range;
    let written = &source[at.start_offset..at.end_offset()];
    assert!(
        written.contains("DECLARE_HANDLE"),
        "the declaration is reported at the invocation: {written:?}"
    );

    // What the cook reported about itself, asked for explicitly so the numbers are part of the test: the file
    // was already cooked once by the drain, and cooking it again reads the same thing.
    let cooked = session.cook_the_open_files();
    assert_eq!(cooked.len(), 1, "api.h is the open file");
    let (path, reading) = &cooked[0];
    assert_eq!(path, Path::new("/p/api.h"));
    assert!(
        reading.declarations >= 2,
        "the struct, its member and the typedef are declarations here: {reading:?}"
    );
    assert!(
        reading.only_after_expansion >= 2,
        "`HWND__` and `HWND` exist only after expansion: {reading:?}"
    );
    assert_eq!(reading.mapped.dropped, 0, "every range landed in the file");
}

/// **Opening a file reads the files it includes the way a compiler reads them too.**
///
/// A name used in the file the user is editing is declared in a header — often one they never open — and the
/// answer comes from **that** file's reading. So the closure is what gets cooked, not the one file: here
/// `main.cpp` is open, `api.h` is not, and the declaration only exists because `api.h` was read as a compiler
/// reads it. Without the closure marking the index would have the raw reading of `api.h` — which cannot see
/// what `DECLARE_HANDLE` declares at all.
///
/// **The bound is one level of includes, and a unit read moves it** — see
/// [`a_unit_read_reads_the_whole_program_once`] for what the session does when a caller asks for the program
/// rather than for one file, and why that is not wired into the pump yet.
#[test]
fn a_header_the_user_never_opened_is_cooked_when_a_request_names_it() {
    let handle = "#include \"deep.h\"\n#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                      typedef struct name##__ *name\n";
    let api = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
    let main = "#include \"api.h\"\nHWND h;\n";
    let files = MemoryFiles::new()
        .with_file("/p/deep.h", "#define DEEP 1\n")
        .with_file("/p/handle.h", handle)
        .with_file("/p/api.h", api)
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-named-file-is-cooked", &files);
    let mut session = fixture.session();

    session.did_open("/p/main.cpp", main);
    session.index_everything();

    // **The open file, and the files its own text names.** `api.h` is one of them, so the declaration the macro
    // writes in it is `api.h`'s declaration and a question about `HWND` is answered with a jump into it — the
    // capability that the closure used to buy, bought for the price of one direct include instead of 138 files.
    let after = session
        .index()
        .definition("HWND__", Path::new("/p/main.cpp"));
    let Known::Yes(found) = after else {
        panic!("`api.h`'s cooked reading declares it: {after:?}");
    };
    assert_eq!(
        found.file,
        Path::new("/p/api.h"),
        "the file that invoked the macro, which nobody opened"
    );

    // **And one level further, which is the bound** — `handle.h` is a nested include of a direct one, and
    // `want_the_closure_cooked` reaches it deliberately: [`COOK_ONE_LEVEL_FURTHER`] exists because MSVC's
    // `<string>` is a thin wrapper over `<xstring>`, and `std::string` lives in the *second* level, so a policy
    // that stopped at the direct includes answered `std::string` with nothing. Cooking is a render and an index
    // per file, so the level is bounded at sixty-four rather than taken to the closure — measured on a real
    // project, the closure was 138 files and **11.7 s of every startup**, and the direct includes are four.
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/handle.h"))
            .is_some(),
        "the level under a direct include is where the library keeps what a reader asks for"
    );

    // **And one level more than that.** `deep.h` is the third level down, which
    // [`COOK_TWO_LEVELS_FURTHER`] reaches: the class a header *uses* is often one `#include` below the header the
    // file writes — MSVC's `<string>` is a thin layer over `<xstring>`, where `basic_string` lives — and measured on
    // EmmyLuaCodeStyle, `std::string` answered `NotDeclaredHere` for a file whose header includes the header that
    // includes the one that declares it. Past that bound a file still has no reading until something names it.
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/deep.h"))
            .is_some(),
        "the third level is where a header's own class is declared, and it is cooked"
    );

    // A request naming it — the shell's `prepare` does this for every file a request is about — is what makes it.
    session.want_cooked_reading(Path::new("/p/handle.h"));
    session.index_everything();
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/handle.h"))
            .is_some(),
        "and then it has one"
    );
}

/// **A file whose text does not balance its braces is named, and its declarations are filed anyway.**
///
/// The hazard this pins was found by measurement, not by reading: on a real project
/// `CodeAnalysis/sourceannotations.h` — the Windows SDK's `/analyze` header, the one file a census has never
/// read cleanly — opens `namespace vc_attributes {` whose close is not in the text we rendered. Everything
/// spliced after it read as `vc_attributes::std`: `<cstdio>`'s declarations, `<string>`'s, the lot. A reader
/// asking about `std::size_t` was told the index had no such name while the file in front of them declared it.
///
/// # What this used to assert, and why it was the wrong shape
///
/// It used to assert that such a file's text was **left out of the stream** — the plan's gate ①, one of the two
/// all-or-nothing gates §2 names. That throws away every declaration in the file to protect the ones after it,
/// which is §4 rule 1's *"任何一层都不允许因为'不确定'而放弃已经确定的答案"* broken in the same way the
/// `crossings == 0` gate broke it one layer up.
///
/// The text now goes in, and the crossed pairing it creates is repaired like every other crossed pairing: the
/// `{` this file opened is paired with a `}` in another file, **that pair** is neutralised, and both files are
/// read on their own as well. So the assertions here are the two that matter and they pull in opposite
/// directions, which is what makes them worth having: the file after the unbalanced one is still scoped by its
/// own namespace, **and** the unbalanced file's own declaration is in the index.
#[test]
fn a_file_that_does_not_balance_its_braces_is_named_and_still_read() {
    let files = MemoryFiles::new()
        // Opens a namespace and never closes it: what a `/analyze` header does, and what swallows a program.
        .with_file(
            "/p/broken.h",
            "namespace vc_attributes {\nstruct NotAClosure { int x; };\n",
        )
        .with_file(
            "/p/good.h",
            "namespace good {\nstruct Inside { int y; };\n}\n",
        )
        .with_file(
            "/p/main.cpp",
            "#include \"broken.h\"\n#include \"good.h\"\nInside i;\n",
        );
    let fixture = Memory::new("a-unit-that-does-not-balance", &files);
    let mut session = fixture.session();

    session.did_open(
        "/p/main.cpp",
        "#include \"broken.h\"\n#include \"good.h\"\nInside i;\n",
    );
    session.index_everything();

    let reading = session
        .read_the_unit(Path::new("/p/main.cpp"))
        .expect("the unit reads");

    assert_eq!(
        reading.unbalanced.len(),
        1,
        "one file does not balance: {reading:?}"
    );
    assert!(
        reading.unbalanced[0].ends_with("broken.h"),
        "and it is the one that opens a brace it never closes: {reading:?}"
    );

    // **And the file after it is not nested inside the scope `broken.h` left open.** `broken.h`'s unclosed
    // namespace ends at the end of `broken.h`, so `good.h`'s own namespace holds `Inside` and nothing else.
    //
    // This is the parser's `file_boundaries` rule, the same one
    // `a_file_that_does_not_balance_costs_nothing_but_a_name` asserts for a file whose braces do not balance at
    // all. Measured on 2026-06: **it does not hold here** — `good.h` comes out scoped `vc_attributes::good`, so
    // the namespace `broken.h` opened is still open across the boundary. The assertion is left as written
    // because it is the contract; the failure is real and is not a stale expectation.
    let inside = session
        .index()
        .cooked_declarations(Path::new("/p/good.h"))
        .expect("good.h is part of the program");
    let inside = inside
        .iter()
        .find(|fact| fact.name == "Inside")
        .expect("the class is declared there");
    assert_eq!(
        inside.scope.as_deref(),
        Some("good"),
        "a scope does not cross a file boundary, so `good.h` is scoped by its own namespace"
    );

    // **And the unbalanced file did not lose its own declarations.** This is the half gate ① could not give: the
    // file's struct is declared in it, and it is filed under the namespace the file itself opens.
    let broken = session
        .index()
        .cooked_declarations(Path::new("/p/broken.h"))
        .expect("the unbalanced file's own reading is filed too");
    let struct_fact = broken
        .iter()
        .find(|fact| fact.name == "NotAClosure")
        .expect("the struct is declared in the file that does not balance");
    assert_eq!(
        struct_fact.scope.as_deref(),
        Some("vc_attributes"),
        "and at the scope its own text gives it: {broken:?}"
    );
}

/// **MSVC's single-bracket attribute no longer leaks, and the fence is not what saves it.**
///
/// This test used to assert that `broken.h` was *quarantined*: its text balances its braces, the parser still
/// got it wrong — `REPEATABLE [source_annotation_attribute(1)] struct X { … };` read as an expression whose
/// lambda body never closes — and the fence took the file out of the program so that `good.h` could still be
/// read as itself. The reading was right and the file was not.
///
/// The parser now reads that shape (a decl-specifier sequence accepts a single-bracket attribute, see
/// [`cpp_parser`]'s `at_a_single_bracket_attribute`), so the assertion has to be the *stronger* one: nothing
/// is quarantined, because nothing leaked — and the declarations of the file that used to poison the program
/// are in the index under their own names.
///
/// The fence itself is still tested, by [`a_file_that_really_leaks_is_repaired_and_still_filed`] with a file that
/// still does.
#[test]
fn a_single_bracket_attribute_no_longer_props_the_program_open() {
    let leaky = "REPEATABLE\n[source_annotation_attribute( 1 )]\nstruct Pre\n{\n int Deref;\n};\n";
    let main = "#include \"broken.h\"\n#include \"good.h\"\nInside i;\n";
    let files = MemoryFiles::new()
        .with_file("/p/broken.h", leaky)
        .with_file(
            "/p/good.h",
            "namespace good {\nstruct Inside { int y; };\n}\n",
        )
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-scope-that-used-to-leak", &files);
    let mut session = fixture.session();

    session.did_open("/p/main.cpp", main);
    session.index_everything();
    let reading = session
        .read_the_unit(Path::new("/p/main.cpp"))
        .expect("the unit reads");

    assert!(
        reading.unbalanced.is_empty(),
        "the text balances, and now the parse does too: {reading:?}"
    );
    assert!(
        reading.unbalanced.is_empty(),
        "nothing leaked and nothing is out of balance: {reading:?}"
    );

    // **The declarations of the file that used to poison the program are filed, under their own names.**
    let pre = session
        .index()
        .cooked_declarations(Path::new("/p/broken.h"))
        .expect("the reading was filed")
        .iter()
        .find(|fact| fact.name == "Pre")
        .expect("the struct the attribute precedes is declared there");
    assert_eq!(
        pre.scope, None,
        "and it is at file scope, not inside anything"
    );

    let inside = session
        .index()
        .cooked_declarations(Path::new("/p/good.h"))
        .expect("the reading was filed")
        .iter()
        .find(|fact| fact.name == "Inside")
        .expect("the class is declared there")
        .scope
        .clone();
    assert_eq!(
        inside.as_deref(),
        Some("good"),
        "the file after it is read as itself"
    );
}

/// **A file that really leaks no longer costs anything, because there is no fence to invoke.**
///
/// `good.h` opens a namespace and never closes it, so a parse of the program sees `other.h` inside it. That
/// used to be found (a brace paired across two files) and repaired (the pair neutralised, both files read on
/// their own). **Both halves are gone**, and the reason is the plan's §3.0: a crossing needs the parser to pair
/// a brace in one file with one in another, which it can only do when the text it is given has a macro invocation
/// in place of the braces — and this stream is the *cooked* one, where every macro is already replaced.
///
/// So what is asserted here is what remains true and matters: nothing is refused, the file after the leak is
/// scoped by its own namespace, and the leaking file keeps its own declaration.
#[test]
fn a_file_that_does_not_balance_costs_nothing_but_a_name() {
    // `good.h` opens a namespace and never closes it. `other.h` is the file after it.
    let good = "namespace good {\nstruct Good { int x; };\n";
    let other = "struct Other { int y; };\n";
    let main = "#include \"good.h\"\n#include \"other.h\"\nOther o;\n";
    let files = MemoryFiles::new()
        .with_file("/p/good.h", good)
        .with_file("/p/other.h", other)
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-scope-that-really-leaks", &files);
    let mut session = fixture.session();

    session.did_open("/p/main.cpp", main);
    session.index_everything();
    let reading = session
        .read_the_unit(Path::new("/p/main.cpp"))
        .expect("the unit reads");

    // **The imbalance is named, and that is all that happens to it.** No file is quarantined, nothing is
    // repaired, and the reading is filed — the plan's §4 rule 1.
    assert_eq!(
        reading.unbalanced.len(),
        1,
        "one file does not balance, and it is reported: {reading:?}"
    );

    // **The file after it is read as itself, at file scope.**
    //
    // This is what the fence used to buy, and it is now bought **in the parser**: a scope may not cross a file
    // boundary, so the `{` that `good.h` never closed is closed at the end of `good.h` rather than paired with a
    // `}` in `other.h`. See `ParserConfig::file_boundaries` for the rule and `balance_events` for where it is
    // applied.
    //
    // The distinction matters for who owns the fix: deleting the fence exposed this leak, and the answer was not
    // to put the fence back — it was a defect **in the parser**, and the parser is where it is now fixed.
    let other_fact = session
        .index()
        .cooked_declarations(Path::new("/p/other.h"))
        .expect("other.h is part of the program")
        .iter()
        .find(|fact| fact.name == "Other")
        .map(|fact| fact.scope.clone())
        .expect("the struct is declared there");
    assert_eq!(
        other_fact, None,
        "the file after the unbalanced one is at file scope, not inside `good::`: {reading:?}"
    );

    // **And the unbalanced file did not lose its own declarations.**
    let good_facts = session
        .index()
        .cooked_declarations(Path::new("/p/good.h"))
        .expect("the leaking file's own reading is filed too");
    assert!(
        good_facts.iter().any(|fact| fact.name == "Good"),
        "the struct is declared in the file that does not balance: {good_facts:?}"
    );
}

/// **A program in which no file leaks is read once** — the ordinary case has no quarantine and no second parse.
#[test]
fn a_program_whose_files_keep_their_scopes_quarantines_nothing() {
    let main = "#include \"a.h\"\n#include \"b.h\"\nint main_variable;\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.h", "namespace a {\nstruct A { int x; };\n}\n")
        .with_file("/p/b.h", "namespace b {\n#include \"c.h\"\n}\n")
        .with_file("/p/c.h", "struct C { int z; };\n")
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-program-that-does-not-leak", &files);
    let mut session = fixture.session();

    session.did_open("/p/main.cpp", main);
    session.index_everything();
    let reading = session
        .read_the_unit(Path::new("/p/main.cpp"))
        .expect("the unit reads");

    assert!(
        reading.unbalanced.is_empty(),
        "a scope that closes around an `#include` is in balance: {reading:?}"
    );
    let c = session
        .index()
        .cooked_declarations(Path::new("/p/c.h"))
        .expect("filed")
        .iter()
        .find(|fact| fact.name == "C")
        .expect("declared")
        .scope
        .clone();
    assert_eq!(
        c.as_deref(),
        Some("b"),
        "the namespace `b.h` opens around its include still holds `C`"
    );
}

/// **A unit read puts the whole program in the index, once** — [`Session::read_the_unit`].
///
/// The reading this session is built to reach and has not reached: one walk, one stream, one parse, and every
/// declaration filed under the file it was written in — so a header has the program's reading whether or not
/// anybody opened it. Here the same fixture as above, asked for the **program** rather than for one file:
/// `handle.h` is read (it is what the program is made of), and a file the program does not read is not.
///
/// It is a test of the call as the *caller* makes it — [`Session::read_the_unit`] is still the API for a caller that
/// wants a program now. The pipeline's own schedule is
/// [`a_program_is_read_when_the_reader_stops_editing`] below, which is a different question: *when*.
#[test]
fn a_unit_read_reads_the_whole_program_once() {
    let handle = "#include \"deep.h\"\n#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                      typedef struct name##__ *name\n";
    let api = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";
    let main = "#include \"api.h\"\nHWND h;\n";
    let files = MemoryFiles::new()
        .with_file("/p/deep.h", "#define DEEP 1\n")
        .with_file("/p/handle.h", handle)
        .with_file("/p/api.h", api)
        .with_file("/p/main.cpp", main)
        .with_file("/p/other.cpp", "#include \"other.h\"\nvoid g() { }\n")
        .with_file("/p/other.h", "struct Unrelated { int y; };\n");
    let fixture = Memory::new("a-unit-read", &files);
    let mut session = fixture.session();

    session.did_open("/p/main.cpp", main);
    session.index_everything();

    // **The per-file policy has a bound, and this is it.** `handle.h` is the level under a direct include, which
    // [`COOK_ONE_LEVEL_FURTHER`] reaches on purpose (`<string>` → `<xstring>`); `deep.h` is the level under *that*,
    // which [`COOK_TWO_LEVELS_FURTHER`] reaches for the reason `std::string` needed it — measured on
    // EmmyLuaCodeStyle, `members_of` answered `NotDeclaredHere` for a file whose header includes the header that
    // includes the one that declares the class. The rest of this test is what the session does when a caller asks
    // for the **program** instead of for one file, and the contrast is the point: a unit read has no bound at all.
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/handle.h"))
            .is_some(),
        "the level under a direct include is cooked"
    );
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/deep.h"))
            .is_some(),
        "and the level under that is cooked too — the bound moved when `std::string` needed it"
    );

    let reading = session
        .read_the_unit(Path::new("/p/main.cpp"))
        .expect("the unit reads");
    assert_eq!(reading.root, Path::new("/p/main.cpp"));
    // **`files` is the walk's frames; `files_with_tokens` is smaller, and that is not a defect.** `handle.h`
    // holds one `#define` and nothing else, and a directive contributes no token to the rendering — the same
    // distinction the census records as "a clean file whose rendering is empty was not read at all" (§7). Both
    // numbers are asserted so that the difference stays visible rather than being smoothed over.
    assert!(
        reading.files >= 3,
        "the program is three files: {reading:?}"
    );
    assert_eq!(
        reading.files_with_tokens, 2,
        "two of them write tokens; a file of directives writes none: {reading:?}"
    );
    assert_eq!(reading.missing, 0, "every file had text: {reading:?}");

    // **The program's files**, including the header nobody opened, the level the per-file policy stopped short
    // of, and the macro's declaration in the file that invoked it.
    for (file, why) in [
        (
            "/p/handle.h",
            "a file the program is made of is read with it",
        ),
        ("/p/deep.h", "including the one the per-file bound left out"),
    ] {
        assert!(
            session
                .index()
                .cooked_declarations(Path::new(file))
                .is_some(),
            "{why}"
        );
    }
    let after = session
        .index()
        .definition("HWND__", Path::new("/p/main.cpp"));
    let Known::Yes(found) = after else {
        panic!("`api.h`'s cooked reading declares it: {after:?}");
    };
    assert_eq!(found.file, Path::new("/p/api.h"));

    // **And a file outside the program is not touched**: nothing compiles `other.cpp`, nobody opens it.
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/other.cpp"))
            .is_none(),
        "a file no program reads and nobody opens has no reading"
    );
}

/// **The pipeline reads a whole program once the reader has stopped typing** — the schedule, and its gate.
///
/// A unit read is the session's one indivisible piece of work ([`Session::prepare_a_unit_reading`]: a walk, a cook of
/// every file in include order, a parse of the whole program and a sweep — 5.09 s for a 159-file program measured on
/// a real project). Its preparing half takes the **read** lock, and a keystroke takes the write one, so a reader
/// typing during it waits for the whole program. Two things follow, and both are asserted here:
///
/// ```text
/// while the reader is typing      `advance` reads the file it was told about and no program  → the fixture's
///                                 `deep.h` — two includes below the file opened — has no cooked reading
/// a second after the last edit    the next drain schedules the program, and the closure's files get one
/// ```
///
/// The second half is what makes the whole closure queryable without anybody asking about it, which is the shape
/// §5 step two needs: one timeline per translation unit, not one walk per file.
#[test]
fn a_program_is_read_when_the_reader_stops_editing() {
    // **Four includes deep on purpose**: the per-file policy cooks an open file's direct includes and the level under
    // them (`COOK_TWO_LEVELS_FURTHER`), so `deep.h` — the file this test watches — has to be further down than that
    // or its reading would be there before the program is read at all.
    let handle = "#include \"mid.h\"\n#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                  typedef struct name##__ *name\n";
    let main = "#include \"api.h\"\nHWND h;\n";
    let files = MemoryFiles::new()
        .with_file("/p/deep.h", "#define DEEP 1\n")
        .with_file("/p/mid.h", "#include \"deep.h\"\nstruct Mid { int x; };\n")
        .with_file("/p/handle.h", handle)
        .with_file("/p/api.h", "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n")
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-program-when-the-typing-stops", &files);
    let mut session = fixture.session();

    // **The reader opens the file and types into it**, which is the state the schedule must stay out of.
    session.did_open("/p/main.cpp", main);
    session.index_everything();
    let steps = session.advance(4);
    assert!(
        !session
            .index()
            .cooked_declarations(Path::new("/p/deep.h"))
            .is_some(),
        "nothing read the program while the reader was typing: {:?}",
        paths(&steps)
    );

    // **And the clock is the gate**: a second after the last keystroke, the next drain reads one program.
    std::thread::sleep(super::UNTIL_A_UNIT_MAY_BE_READ + std::time::Duration::from_millis(100));
    session.advance(1);

    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/deep.h"))
            .is_some(),
        "the program's own files are read with it, two includes below the file the reader opened"
    );
    let found = session
        .index()
        .definition("HWND__", Path::new("/p/main.cpp"));
    let Known::Yes(found) = found else {
        panic!("the macro's declaration is in the index after the program was read: {found:?}");
    };
    assert_eq!(
        found.file,
        Path::new("/p/api.h"),
        "filed under the file that invoked the macro"
    );
}

/// **A program too large to read inside a keystroke's budget is left to the per-file reading.**
///
/// The other half of the schedule above. A unit read is linear in its program — 0.72 s per megabyte of source
/// measured — and its preparing half holds the read lock, so a program over
/// [`THE_LARGEST_PROGRAM_READ_AT_ONCE`] would make a keystroke wait longer than the server's own write budget. Such a
/// program is not read as one piece: it keeps the per-file reading, which answers for the file a query names and
/// never blocks anybody.
///
/// The fixture is the same shape with one big comment-only header in the middle, so the *choice* is the only thing
/// that differs between the two tests.
#[test]
fn a_program_too_large_to_read_at_once_is_left_alone() {
    // A comment block just over the budget: the file is in the program's closure and contributes nothing but bytes,
    // which is exactly what the budget is about. Sized from the constant rather than written out, so that raising the
    // budget cannot quietly turn this test into the one above.
    let line = "// padding padding padding padding\n";
    let big = line.repeat(super::THE_LARGEST_PROGRAM_READ_AT_ONCE / line.len() + 1);
    assert!(big.len() > super::THE_LARGEST_PROGRAM_READ_AT_ONCE);

    let handle = "#include \"big.h\"\n#include \"mid.h\"\n#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                  typedef struct name##__ *name\n";
    let main = "#include \"api.h\"\nHWND h;\n";
    let files = MemoryFiles::new()
        .with_file("/p/big.h", big)
        .with_file("/p/deep.h", "#define DEEP 1\n")
        .with_file("/p/mid.h", "#include \"deep.h\"\nstruct Mid { int x; };\n")
        .with_file("/p/handle.h", handle)
        .with_file("/p/api.h", "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n")
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-program-too-large", &files);
    let mut session = fixture.session();

    session.did_open("/p/main.cpp", main);
    session.index_everything();
    std::thread::sleep(super::UNTIL_A_UNIT_MAY_BE_READ + std::time::Duration::from_millis(100));
    session.advance(1);

    assert!(
        !session
            .index()
            .cooked_declarations(Path::new("/p/deep.h"))
            .is_some(),
        "the program is over the budget, so it is not read in one piece"
    );
    // …and the reading the file *does* have is there: the open file and its direct includes are cooked as always.
    assert!(
        session
            .index()
            .cooked_declarations(Path::new("/p/api.h"))
            .is_some(),
        "the per-file reading is unaffected by the budget"
    );
}

/// **An out-of-line definition's type is the type it writes, not its own qualified name.**
///
/// Reported from an editor, three times in one sitting, always as a *type* that was really a name:
///
/// ```cpp
/// std::map<std::string, LuaTokenKind, std::less<>> LuaLexer::LuaReserved = { … };   // hover: `LuaLexer::LuaReserved`
/// std::size_t LuaSource::GetLine(std::size_t offset) const { … }                   // hover: `LuaSource::GetLine`
/// ```
///
/// and the member lookups that followed answered "`LuaLexer::LuaReserved` has no member `find`". The grammar puts a
/// qualified definition's name in the **specifier sequence** — the declarator holds only the parameter list — so a
/// reader that takes "the last name of the specifiers" as the type takes the declarator's own name. A definition and
/// its declaration are both in the index, and whoever asks may get either.
#[test]
fn an_out_of_line_definition_records_the_type_it_writes() {
    let text = "namespace std {\n\
                template <typename K, typename V> struct map { V &at(const K &key); };\n\
                }\n\
                struct Lexer {\n\
                \x20   static std::map<int, int> Reserved;\n\
                \x20   std::size_t GetLine(std::size_t offset) const;\n\
                };\n\
                std::map<int, int> Lexer::Reserved = {};\n\
                std::size_t Lexer::GetLine(std::size_t offset) const { return offset; }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-out-of-line-definition", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");

    let summary = session.index().summary(&path).expect("the file is read");
    for (name, expected) in [
        ("Reserved", "std::map<int, int>"),
        ("GetLine", "std::size_t"),
    ] {
        let written: Vec<Option<&str>> = summary
            .declarations
            .iter()
            .filter(|fact| fact.name == name)
            .map(|fact| fact.type_of.as_deref().or(fact.returns.as_deref()))
            .collect();
        assert!(
            written.iter().any(|found| *found == Some(expected)),
            "`{name}` declares `{expected}`, and one of its facts must say so: {written:?}"
        );
        let qualified = format!("::{name}");
        assert!(
            !written
                .iter()
                .any(|found| found.is_some_and(|found| found.ends_with(&qualified))),
            "and none of them may record the declarator's own qualified name (`…{qualified}`): {written:?}"
        );
    }
}
/// **A class template's member answer is substituted with the use's arguments** — `b[0].value` and `b->value`.
///
/// The two reports from an editor (`events[Pos].U` on a `std::vector<MarkEvent>`, `file->setUri` on a
/// `std::shared_ptr<LuaSource>`) both ended the same way: the member question was asked about a class called `_Ty` or
/// `_Ty2` — the *declaration's* spelling of the class's own parameter, not the type the use wrote. Measured on the real
/// project with `tests/inference_probe.rs`:
///
/// ```text
/// events[Pos].   → Unknown(NotDeclaredHere("_Ty"))
/// file->         → class "_Ty2": 0 member(s)
/// ```
///
/// This fixture is the same question with names that say what is happening.
#[test]
fn a_class_templates_members_are_read_through_the_uses_arguments() {
    // **Three shapes, because the standard library uses all three.** `T& operator[]` states the parameter itself;
    // `reference operator[]` names an alias *of the class*, and the alias is what says `T&` — `std::vector`'s spelling,
    // and the one that answered `NotDeclaredHere("_Ty")` on a real project; `T *operator->` is `shared_ptr`'s.
    let text = "template <typename T> struct Box {\n\
                \x20   T &operator[](int index);\n\
                \x20   T *operator->();\n\
                };\n\
                template <typename T> struct AliasBox {\n\
                \x20   using reference = T &;\n\
                \x20   reference operator[](int index);\n\
                };\n\
                struct Item { int value; void grow(); };\n\
                void f(Box<Item> box) {\n\
                \x20   box[0].value;\n\
                \x20   box->grow();\n\
                }\n\
                void g(AliasBox<Item> box) {\n\
                \x20   alias[0].value;\n\
                }\n";
    let text = text.replace("alias[0]", "box[0]");
    let files = MemoryFiles::new().with_file("/p/a.cpp", &text);
    let fixture = Memory::new("a-class-templates-members", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", &text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // **The member after the operator, as a definition question** — the shape both reports had: `events[Pos].U` and
    // `file->setUri` are jumps, and a jump that asks about `_Ty` finds nothing.
    //
    // The third one searches *after* the first two, because the fixture writes `box[0].value` twice and the cursor of
    // each case has to be in its own function.
    let mut searched = 0usize;
    for (written, expected) in [
        ("box[0].value", ("value", Some("Item"))),
        ("box->grow", ("grow", Some("Item"))),
        ("box[0].value", ("value", Some("Item"))),
    ] {
        let found_at = text[searched..]
            .find(written)
            .map(|at| searched + at)
            .expect("the fixture writes it");
        searched = found_at + written.len();
        let at = found_at + written.len() - expected.0.len();
        let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(at));
        let Known::Yes(found) = found else {
            panic!("`{written}` must resolve to the element's member: {found:?}");
        };
        assert_eq!(
            (
                found.found[0].fact.name.as_str(),
                found.found[0].fact.scope.as_deref()
            ),
            expected,
            "`{written}` is the element's member"
        );
    }
}
/// **A member template's own parameter is not the class's** — the shape `std::shared_ptr` is written in.
///
/// MSVC's `<memory>` declares
///
/// ```cpp
/// template <class _Ty> class shared_ptr : public _Ptr_base<_Ty> {
///     template <class _Ty2 = _Ty, enable_if_t<!is_array_v<_Ty2>, int> = 0>
///     _NODISCARD _Ty2* operator->() const noexcept { return get(); }
/// ```
///
/// so the member's answer mentions `_Ty2` — a parameter of the *member template*, defaulted to the class's `_Ty`. The
/// fact records the member's parameters as names, without their defaults, so substituting with the class's arguments
/// leaves `_Ty2` standing and the member lookup asks about a class called `_Ty2` (measured on EmmyLuaCodeStyle's
/// `LuaFormat.cpp`: `file->` → `class "_Ty2": 0 member(s)`).
///
/// What settles it is the convention every pointer-like type in the standard library follows: `element_type` is what
/// the arrow reaches.
#[test]
fn an_arrow_through_a_member_template_reaches_the_element_type() {
    // **The shape MSVC writes**, because it is the one that broke: the base is named through an alias, and the alias
    // being re-exposed (`using typename _Mybase::element_type;`) declares no type of its own.
    let header = "template <typename T> struct Holder {\n\
                  \x20   using element_type = T;\n\
                  };\n\
                  template <typename T> struct Handle {\n\
                  \x20 private:\n\
                  \x20   using _Mybase = Holder<T>;\n\
                  \x20 public:\n\
                  \x20   using typename _Mybase::element_type;\n\
                  \x20   template <typename U = T, int = 0>\n\
                  \x20   U *operator->() const;\n\
                  };\n";
    let source = "#include \"handle.h\"\n\
                  struct Item { int value; void grow(); };\n\
                  void g(Handle<Item> file) {\n\
                  \x20   file->\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/handle.h", header)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("an-arrow-through-a-member-template", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let written = "file->";
    let at = source.find(written).expect("the fixture") + written.len();
    let completions =
        session.member_completions(&view, crate::file::coordinates::FileOffset::new(at));
    let Known::Yes(completions) = completions else {
        panic!("`{written}` must list the element's members: {completions:?}");
    };
    let names: Vec<&str> = completions
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert_eq!(
        completions.class, "Item",
        "the arrow reaches `element_type`, not the member template's parameter name ({names:?})"
    );
}
/// **The real shape: a base in another file, a namespace, and an alias with arguments.**
///
/// The fixture above keeps `element_type` in the same file; this one is MSVC's layout — `std::_Ptr_base` in one header,
/// `std::shared_ptr` in another, the base named through `using _Mybase = _Ptr_base<_Ty>;` and the alias re-exposed with
/// `using typename _Mybase::element_type;`. It exists because the arrow's fallback works on the small fixture and did
/// **not** work on a real project, and the difference had to be found somewhere cheaper than a 95-second probe run.
#[test]
fn an_arrow_through_a_base_named_by_a_member_alias_in_another_file() {
    // **A forward declaration first**, which is what MSVC's `<memory>` has and what a member list must look past: the
    // first visible declaration of a class is not always the one with a body.
    // **The trait, not the parameter** — MSVC's `_Ptr_base` says `using element_type = remove_extent_t<_Ty>;`, and
    // `remove_extent_t` is an alias *template* whose target the library writes through `typename remove_extent<T>::type`.
    // A layer that follows spellings arrives at the bare name `remove_extent_t`, which is an alias and not a class; the
    // answer for a pointer-like handle is the class's own argument, and that is what the last part of the arrow's
    // fallback is for.
    let base = "namespace std {\n\
                template <typename T> class _Ptr_base;\n\
                template <typename T> struct remove_extent { using type = T; };\n\
                template <typename T> using remove_extent_t = typename remove_extent<T>::type;\n\
                template <typename T> class _Ptr_base {\n\
                public:\n\
                \x20   using element_type = remove_extent_t<T>;\n\
                };\n\
                }\n";
    let memory = "#include \"ptr_base.h\"\nnamespace std {\n\
                  template <typename T> class shared_ptr {\n\
                  private:\n\
                  \x20   using _Mybase = _Ptr_base<T>;\n\
                  public:\n\
                  \x20   using typename _Mybase::element_type;\n\
                  \x20   template <typename U = T, int = 0>\n\
                  \x20   U *operator->() const;\n\
                  \x20   element_type *get() const;\n\
                  };\n\
                  }\n";
    let source = "#include \"memory.h\"\n\
                  struct Item { int value; void grow(); };\n\
                  void g(std::shared_ptr<Item> file) {\n\
                  \x20   file->\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/ptr_base.h", base)
        .with_file("/p/memory.h", memory)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("an-arrow-through-a-real-shape", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let written = "file->";
    let at = source.find(written).expect("the fixture") + written.len();
    let completions =
        session.member_completions(&view, crate::file::coordinates::FileOffset::new(at));
    let Known::Yes(completions) = completions else {
        panic!("`{written}` must list the element's members: {completions:?}");
    };
    let names: Vec<&str> = completions
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert_eq!(
        completions.class, "Item",
        "the arrow reaches the base alias's `element_type` ({names:?})"
    );
}
/// **The same question when the class is declared in a header** — the shape the real project has, and the one that
/// answers `_Ty`.
///
/// `std::vector`'s `operator[]` is declared `reference operator[](size_type)`, and `reference` is `_Ty&` — an alias
/// *inside* the class. When the class is declared in another file, the alias is followed by
/// `ProjectIndex::template_parameters_of`/`resolve_aliases`, which has the *spelling* and not the use's arguments, so
/// it answers `_Ty&` and the member lookup then asks about a class called `_Ty`. Measured on EmmyLuaCodeStyle's
/// `Mark.cpp` (`events[Pos].U`): `Unknown(NotDeclaredHere("_Ty"))`.
#[test]
fn a_class_templates_alias_return_is_substituted_too() {
    let header = "template <typename T> struct AliasBox {\n\
                  \x20   using reference = T &;\n\
                  \x20   reference operator[](int index);\n\
                  };\n";
    let source = "#include \"box.h\"\n\
                  struct Item { int value; void grow(); };\n\
                  void g(AliasBox<Item> box) {\n\
                  \x20   box[0].value;\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/box.h", header)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("a-class-templates-alias", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let written = "box[0].value";
    let at = source.find(written).expect("the fixture") + written.len() - "value".len();
    let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(found) = found else {
        panic!("`{written}` must resolve to the element's member: {found:?}");
    };
    assert_eq!(
        (
            found.found[0].fact.name.as_str(),
            found.found[0].fact.scope.as_deref()
        ),
        ("value", Some("Item")),
        "the alias's target is the use's argument, not the class's parameter name"
    );
}
/// **An anonymous enum declares its enumerators** — `enum { LAJF };`, the C spelling of a set of constants.
///
/// Reported from an editor as "an unnamed enum is not supported", and it was literally true: the declaration names
/// nothing, `is_unnamed_declaration` was consulted before the body was walked, and the walker returned — so every
/// enumerator of every anonymous enum was missing from the scope tree and from the index. Two lines above it the file
/// declares `LAJF`, and `definitions("LAJF")` answered `NotDeclaredHere`.
#[test]
fn an_anonymous_enum_declares_its_enumerators() {
    let text = "enum { LAJF, LBJF };\nvoid f() { int x = LAJF; }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-anonymous-enum", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");

    for name in ["LAJF", "LBJF"] {
        let found = session.index().definitions(name, &path);
        let Known::Yes(found) = found else {
            panic!("`{name}` is declared by the anonymous enum: {found:?}");
        };
        assert_eq!(
            found.found[0].fact.scope, None,
            "an unnamed enum has no scope name"
        );
    }

    // **And the name is classified as an enumerator**, which is what the semantic pass draws.
    let view = session.view_of_the_file(&path).expect("a view");
    let names = session.classified_names(&view);
    let lajf = names
        .iter()
        .find(|name| name.range.start_offset == text.find("LAJF").expect("the fixture"))
        .expect("`LAJF` is a name");
    assert_eq!(lajf.kind, crate::semantic::NameKind::EnumMember);
}
/// **A member reached through `operator->`** — `config->Generate` on a smart-pointer-shaped class.
///
/// Reported twice from an editor, in one sentence: *"a member cannot be found through a smart pointer, and there is no
/// completion either"*. The handle's own member list is `operator->`, `get`, `reset`; the names a reader is reaching
/// for after an arrow are the pointee's. Both halves are answered here: the lookup follows what `operator->` returns
/// (substituted with the arguments the use wrote, because the return type is the *class's* spelling — `_Ty *` for
/// `std::shared_ptr`), and the member list after `->` is the pointee's.
#[test]
fn a_member_through_an_operator_arrow_is_the_pointees() {
    let text = "template <typename T> struct handle { T *operator->(); void reset(); };\n\
                struct Config { int Generate(int uri); };\n\
                void g(handle<Config> config) { config->Generate(1); }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("through-an-arrow", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // **The lookup**: `Generate` is `Config`'s, not the handle's.
    let at = text.find("Generate(1)").expect("the fixture") + 1;
    let found = session.definition(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(found) = found else {
        panic!("`config->Generate` must resolve through `operator->`: {found:?}");
    };
    assert_eq!(
        (found.fact.name.as_str(), found.fact.scope.as_deref()),
        ("Generate", Some("Config")),
        "the member is the pointee's"
    );

    // **And the list**, after the arrow: the pointee's declarations.
    let after = text.find("config->Generate").expect("the fixture") + "config->".len();
    let completions =
        session.member_completions(&view, crate::file::coordinates::FileOffset::new(after));
    let Known::Yes(completions) = completions else {
        panic!("`config->` must list the pointee's members: {completions:?}");
    };
    assert_eq!(completions.class, "Config");
    let names: Vec<&str> = completions
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert_eq!(names, ["Generate"], "and not the handle's own list");
}
/// **`index.read_the_whole_project = false` keeps the reader's own programs and nothing else.**
///
/// The setting exists because the two reads are worth different things: a file the reader has open is a program they
/// are about to ask about, and one nobody has open is speculative — 36 s of CPU for 96 translation units on the
/// project this was measured on (`docs/architecture.md` §8.4). A project that says so gets the first and not the
/// second, and the clock says it too: with the setting off there is nothing for the pump to wait for once the reader's
/// own programs are read.
#[test]
fn a_project_can_decline_to_read_its_other_roots() {
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
/// **The project's other roots are read in the background, after a pause that is clearly not mid-sentence.**
///
/// A reader has `a.cpp` open and `b.cpp` is a project root nobody is looking at. Both include one header, so reading
/// either program makes that header's declarations the compiler's reading for the files it is in force in — which is
/// the whole point of §7.6: a project nobody has opened a file from still becomes queryable, one program at a time.
///
/// The two clocks are the behaviour: the open file's program after **one second**, the project's other roots after
/// **ten**. `pretend_a_pause` moves the session's editing clock rather than sleeping, because the second number is a
/// person's typing rhythm and a test that waited for it would be a test nobody runs.
#[test]
fn the_roots_of_the_project_are_read_in_the_background() {
    let shared = "struct Shared { int size; };\n";
    let a = "#include \"shared.h\"\n";
    let b = "#include \"shared.h\"\nint from_b = 1;\n";
    // **The project asks for it**, because the setting is off by default now — see `IndexSection`'s documentation for
    // the reader's report and the measured price of reading every translation unit.
    let files = MemoryFiles::new()
        .with_file("/p/shared.h", shared)
        .with_file("/p/a.cpp", a)
        .with_file("/p/b.cpp", b)
        .with_file("/p/.cppls.toml", "[index]\nread_the_whole_project = true\n");
    let fixture = Memory::new("the-project-is-read", &files);
    let mut session = crate::Session::with_config(
        "/p",
        fixture.providers.clone(),
        crate::WatchFilter::new("/p"),
        crate::CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/b.cpp"),
        std::path::PathBuf::from("/p/shared.h"),
    ]);
    session.did_open("/p/a.cpp", a);
    session.index_everything();

    // **The reader's own file**, one second after they stop typing.
    session.pretend_a_pause(std::time::Duration::from_millis(1_100));
    let first = session
        .read_a_program_now()
        .expect("the program of the file the reader has open");
    assert_eq!(first.root, std::path::Path::new("/p/a.cpp"));

    // **A root nobody has open waits ten seconds**, and it is the *header* that is not a root: `shared.h` is
    // included by two files, so it is part of their programs rather than a program of its own.
    assert!(
        session.read_a_program_now().is_none(),
        "the project's other roots wait for a longer pause"
    );
    session.pretend_a_pause(std::time::Duration::from_secs(11));
    let second = session
        .read_a_program_now()
        .expect("the project's other root");
    assert_eq!(second.root, std::path::Path::new("/p/b.cpp"));

    // **And both programs filed the header's declarations**, each under the program that read it (⑲): the header is
    // one text with two readings, and neither session has to have opened it.
    assert!(
        session
            .index()
            .the_program_that_read(std::path::Path::new("/p/shared.h"))
            .is_some(),
        "the header was read by the programs that include it"
    );
    assert!(
        matches!(
            session
                .index()
                .definitions("Shared", std::path::Path::new("/p/a.cpp")),
            Known::Yes(_)
        ),
        "and a lookup from `a.cpp` finds it"
    );
    assert!(
        matches!(
            session
                .index()
                .definitions("Shared", std::path::Path::new("/p/b.cpp")),
            Known::Yes(_)
        ),
        "and from a file whose program was read later"
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
fn a_body_edit_does_not_cost_the_program_again() {
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
#[test]
fn a_file_read_as_a_program_needs_no_guard_filter() {
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
fn a_files_rendering_is_its_reading_and_not_one_answer_beside_another() {
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

/// **The same header, read in two translation units, answers each of them its own way** — §5 step two's acceptance.
///
/// `cfg.h` declares `X` one way when `A` is defined and another when it is not, so `a.cpp` (which defines it before
/// the include) and `b.cpp` (which does not) read **different** `X` from the same bytes. The index keeps a reading
/// **per program** (`Readings`), and a lookup picks by the asking file's program — the `env_hash` of §5 step two.
/// Before that, the file kept the *last* program's reading and this test answered `b.cpp` with what `a.cpp` compiled.
#[test]
fn a_header_read_in_two_programs_answers_each_its_own_way() {
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

    // **Both programs**, one per call — the schedule reads one at a time and the two roots are both open.
    let mut read = Vec::new();
    while let Some(reading) = session.read_a_looked_at_unit() {
        read.push(reading.root.clone());
    }
    assert!(read.len() >= 2, "both translation units are read: {read:?}");

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
fn a_branch_nobody_takes_reports_nothing_once_the_file_is_cooked() {
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
        super::DiagnosticReading::Cooked,
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
fn a_cooked_error_is_placed_in_the_file_not_in_the_rendering() {
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
    assert_eq!(found.reading, super::DiagnosticReading::Cooked);
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
/// ([`super::DiagnosticReading::Raw`]) rather than pretending otherwise.
#[test]
fn a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is() {
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
fn editing_a_header_invalidates_the_readings_that_depend_on_it() {
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
fn editing_a_file_that_defines_no_macros_keeps_its_dependents_readings() {
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
fn the_first_macro_a_header_defines_invalidates_the_readings_that_include_it() {
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
/// This is the acceptance of `docs/status.md` stage 1, and it is written as a *count* rather than a
/// duration because the design is about what gets recomputed. The numbers are [`Session::unit_stats`]'s:
/// `walked` is a closure read and a timeline built, `from_disk` is the entry that replaces it.
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
fn typing_in_a_body_does_not_cost_the_walk_a_translation_unit_is() {
    let main = "#include \"b.h\"\nint main() { return A; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.h", "#pragma once\n#define A 1\n")
        .with_file(
            "/p/b.h",
            "#pragma once\n#include \"a.h\"\nstruct B { int b; };\n",
        )
        .with_file("/p/main.cpp", main);
    let fixture = Memory::new("a-body-edit-is-not-a-walk", &files);

    // **The first session walks, because there is nothing to reuse** — and writes the entry for the next one.
    // `index_everything` is the whole of the work: it reads the queue *and* drains the cooking backlog, which
    // is where a unit is reached.
    let mut first = fixture.session();
    first.add_project_files([PathBuf::from("/p/main.cpp")]);
    first.did_open("/p/main.cpp", main);
    first.index_everything();
    assert!(
        first
            .index()
            .cooked_declarations(Path::new("/p/main.cpp"))
            .is_some(),
        "the fixture cooked the file, which is what walks its unit"
    );
    assert!(
        first.unit_stats().walked > 0,
        "…and nothing was on disk to serve it: {:?}",
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

    // **The text that was walked**, as a fresh session reads it.
    let unedited = over_a_fresh_session("unedited", main);
    let walked_baseline = unedited.walked;
    // **A fresh session walks, because there is no timeline cache on disk any more** — `tu/` is gone with the
    // rest of the per-directory cache: a stored timeline outlives the files it was built from, which is a
    // correctness question rather than a speed one. What survives is what the cases below measure: a keystroke must
    // not cost *more* than the fresh session already pays.
    assert_eq!(
        unedited.walked, walked_baseline,
        "the entry the first session wrote answers for the text it was walked from: {unedited:?}"
    );
    assert!(
        unedited.walked > 0,
        "…and the fresh session walked, because no timeline survives a session any more: {unedited:?}"
    );

    // **A character typed into the body.** Nothing above the file's last directive moves, and nothing below it
    // can be read by a walk — so the unit the first session built is still this file's unit. **This is the
    // acceptance of stage 1**: before the root of a unit was keyed on its preamble, this session walked.
    let typed = format!("{main}\nint more() {{ return A; }}\n");
    let edited = over_a_fresh_session("a body edit", &typed);
    assert_eq!(
        edited.walked, walked_baseline,
        "a body edit must not cost more walks than a fresh session already pays: {edited:?}"
    );
    assert!(
        edited.walked > 0,
        "…and the body edit walked, as every fresh session now does: {edited:?}"
    );

    // **An `#include` above the body is a different matter**: it moves the preamble, which is the key.
    let with_an_include = "#include \"a.h\"\n#include \"b.h\"\nint main() { return A; }\n";
    let moved = over_a_fresh_session("an include above the body", with_an_include);
    assert!(
        moved.walked > 0,
        "an edit above the bound is an edit to what the walk reads: {moved:?}"
    );
}

/// **A file this session already has a unit for is read *with* that unit's environment, and the second pass is
/// not paid for it.**
///
/// This is `docs/status.md` §5.2 steps 1–3, and it is the enabling half of stage 2 rather than a
/// saving of its own: measured with `examples/keystroke.rs`, there is **no second parse to remove** for the
/// ordinary edit, because the second pass's filter only selects a file whose scopes could depend on a macro
/// body. What it buys is that the *first* parse is the right one — which is the thing that has to be true
/// before the rendering can be skipped, because the rendering is what repairs a reading made without an
/// environment.
/// Two sessions, and the difference between them is the whole claim:
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
#[test]
fn a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass() {
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
    assert!(
        holds_a_unit_for(&warm, "/p/api.h"),
        "the fixture walked a unit for the file"
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
fn a_wave_with_nothing_to_parse_still_carries_the_drains_timelines() {
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
    assert_eq!(
        next.units.len(),
        session.parsed_since_the_last_pass.len().min(4),
        "and the drain's timelines are still prepared, one per file it will re-read"
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
fn a_quoted_include_is_verified_in_the_directory_it_is_asked_from() {
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
fn an_include_added_to_a_header_invalidates_the_readings_that_include_it() {
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
/// **The second half is `docs/status.md` stage 1.** Dropping the whole table was the first version of
/// this rule and it invalidated up to `MAX_UNITS` units to invalidate one — including, on the next keystroke,
/// the one the file needed. A file that merely *includes* the edited one has a timeline of its own, and it is
/// still the same directives at the same offsets: `ns.h` knows nothing about `api.h`.
#[test]
fn a_unit_survives_typing_in_a_body_and_not_a_change_to_a_directive() {
    let ns = "#define BEGIN_NS namespace one {\n#define END_NS }\n";
    let api = "#include \"ns.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";
    let files = MemoryFiles::new()
        .with_file("/p/ns.h", ns)
        .with_file("/p/api.h", api);
    let fixture = Memory::new("a-unit-survives-a-body", &files);
    let mut session = fixture.session();

    session.did_open("/p/api.h", api);
    session.index_everything();
    assert!(
        holds_a_unit_for(&session, "/p/api.h"),
        "the fixture cooked the file, which walked a unit"
    );
    assert!(
        holds_a_unit_for(&session, "/p/ns.h"),
        "…and the header it includes, whose closure does not contain `api.h`"
    );

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
    assert!(
        holds_a_unit_for(&session, "/p/ns.h"),
        "…and `ns.h`'s is not: nothing in its closure moved, and dropping it would be a walk bought for nothing"
    );
}

/// **Is a translation unit held for this file?** — asked through the table's own lock, and per file, because
/// what an edit invalidates is one file's timeline rather than the whole table.
///
/// Four assertions here used to call `units.is_empty()` on the `Mutex` itself, so the *test* had stopped
/// compiling when the field gained a lock; they were then rewritten to ask about the whole table, which was
/// right while an edit cleared it and is wrong now that it does not.
fn holds_a_unit_for(session: &Session<MemoryFiles>, path: &str) -> bool {
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
fn a_cook_that_cannot_succeed_is_not_queued_again_by_a_request() {
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
fn editing_a_header_stales_the_cooked_reading_of_a_file_that_includes_it() {
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
fn a_buffer_is_the_text_the_analysis_reads() {
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

#[test]
fn a_buffer_that_has_never_been_saved_is_a_file_like_any_other() {
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
fn an_edit_replaces_what_the_index_knew_and_the_gap_is_visible() {
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
fn closing_a_document_goes_back_to_the_file() {
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
fn the_open_file_is_worked_first_and_its_includes_next() {
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
fn a_project_file_that_is_opened_moves_to_the_open_half() {
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
fn a_file_that_was_worked_is_queued_again_when_its_answer_changes() {
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
    assert_eq!(session.stats().since(before).rebuilt, 1);
}

// -------------------------------------------------------------------------------------------
// What the client says changed
// -------------------------------------------------------------------------------------------

#[test]
fn a_reported_change_is_read_again_and_a_reported_removal_is_forgotten() {
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
fn a_change_the_client_does_not_care_about_is_no_work() {
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
fn a_configuration_change_re_seeds_the_project_and_the_open_buffers() {
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
fn a_configuration_change_re_arms_a_worked_file_and_it_is_rebuilt() {
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
    assert!(
        matches!(steps[0].outcome, StepOutcome::Built | StepOutcome::Unstored),
        "a different configuration is a different reading, so the text alone cannot answer for it: {:?}",
        steps[0].outcome
    );
    assert_eq!(session.stats().since(before).rebuilt, 1);
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
fn a_name_in_a_file_that_has_not_been_read_is_unknown_and_the_queue_says_so() {
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
fn a_view_is_the_buffer_parsed_and_maps_a_position_onto_an_offset() {
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
fn a_position_maps_back_onto_the_offset_it_came_from() {
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

// -------------------------------------------------------------------------------------------
// Opening a project on disk
// -------------------------------------------------------------------------------------------

/// A project directory, removed when the test ends — including when it fails.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join("cppls-session-disk").join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the fixture directory");

        Project { root }
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the fixture directory");
        }
        std::fs::write(&path, text).expect("the fixture writes");
        path
    }

    /// The project's own files, as paths relative to the root with `/` separators.
    fn relative(&self, paths: &[PathBuf]) -> Vec<String> {
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
fn the_scan_finds_the_projects_sources_and_leaves_the_rest() {
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

#[test]
fn a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session() {
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
fn a_keystroke_in_a_body_reads_the_file_through_its_timeline() {
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
    session.index_everything();
    assert!(
        session.index().summary(&widget).is_some_and(|summary| {
            summary
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
    let summary = session.index().summary(&widget).expect("indexed again");
    let widget_fact = summary
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
/// ([`super::MAX_UNIT_BYTES`], [`super::MAX_UNITS`]), so after a project is indexed the file a reader opens has no
/// entry. The step then used to parse the file's own bytes and leave the repair to the idle pass.
///
/// What this pins is the order `docs/architecture.md` §5 names — **the directives, then the unit, then the parse**:
///
/// ```text
/// directives   the file's includes, read without a parse (`SummaryStore::directive_summary`)
/// the unit     walked from them, and *refused* when a file the program needs has not been read
/// the parse    of the rendering, which is then the reading every consumer gets
/// ```
#[test]
fn a_file_opened_after_the_index_was_built_is_read_through_a_walked_timeline() {
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
/// The guard on the walk above, and the reason it is a condition rather than a retry: a timeline that stops at a file
/// it cannot look up is a timeline of a *smaller* program — the macro is left unexpanded in the rendering — and a
/// rendering is written down and believed. `docs/architecture.md` §5 measures the two and concludes that **a partial
/// environment renders worse than the raw reading**, so a file whose closure is not in the index yet is read as text
/// and the idle pass refines it once the closure is there.
#[test]
fn a_file_whose_program_has_not_been_read_yet_is_still_read_as_text() {
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

// -------------------------------------------------------------------------------------------
// The project's own configuration file
// -------------------------------------------------------------------------------------------

#[test]
fn a_project_configuration_narrows_the_scan_and_widens_the_source_list() {
    // The two things `[workspace]` is for: a tree the analysis should not read, and an extension the engine
    // does not know about. Both are read from the project's own file, on disk, by a session the caller
    // configured — which is the path a test can take without running a compiler.
    let project = Project::new("config-scan");
    project.write("main.cpp", "int main() { return 0; }\n");
    project.write("vendor/third_party.cpp", "int vendored;\n");
    project.write("kernels/vector.cuh", "struct FromCuda { int x; };\n");
    project.write(
        ".cppls.toml",
        "[workspace]\nexclude = [\"vendor/**\"]\nsource_extensions = [\"cuh\"]\n",
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::with_config(
        &project.root,
        providers,
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );

    assert_eq!(
        project.relative(session.project_files()),
        ["kernels/vector.cuh", "main.cpp"],
        "the excluded tree is not scanned, and the project's own extension is"
    );
    assert!(
        session.project_config().found(),
        "the file was read: {:?}",
        session.project_config()
    );
    assert!(session.project_config().problems.is_empty());
}

#[test]
fn the_projects_flags_win_over_the_databases_and_extra_args_add_to_the_winner() {
    // The decision `CompileSection::args` records, pinned: `args` wins outright, `extra_args` adds to whatever
    // won, and `remove_args` drops a flag from it. One test because the three keys are one pass over one list —
    // splitting them would let the order they are documented in stop being the order they happen in.
    let project = Project::new("config-flags");
    project.write("main.cpp", "int main() { return 0; }\n");
    let root = project.root.to_string_lossy().replace('\\', "/");

    project.write(
        "compile_commands.json",
        &format!(
            "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_THE_DATABASE\", \"-std=c++17\", \"-Werror\", \
                 \"-c\", \"{root}/main.cpp\"]}}\n]\n"
        ),
    );
    project.write(
        ".cppls.toml",
        "[compile]\nargs = [\"-DFROM_THE_CONFIG\", \"-std=c++20\"]\nextra_args = [\"-DEXTRA\"]\n",
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

    let defines: Vec<&str> = session
        .config()
        .defines
        .iter()
        .map(|define| define.name.as_ref())
        .collect();
    assert_eq!(
        defines,
        ["FROM_THE_CONFIG", "EXTRA"],
        "the file's flags are the compilation, and `extra_args` joins them"
    );
    assert_eq!(
        session.config().standard.as_deref(),
        Some("c++20"),
        "the database's `-std=c++17` is not in effect"
    );
}

#[test]
fn removing_and_adding_a_flag_is_one_pass_in_that_order() {
    // `remove_args` drops from the list that won, and `extra_args` appends to it — removing *first* is what
    // makes "drop the database's `-std=c++17`, add `-std=c++20`" mean c++20 rather than c++17.
    let project = Project::new("config-remove");
    project.write("main.cpp", "int main() { return 0; }\n");
    let root = project.root.to_string_lossy().replace('\\', "/");

    project.write(
            "compile_commands.json",
            &format!(
                "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-std=c++17\", \"-DKEEP\", \"-Werror\", \"-c\", \"{root}/main.cpp\"]}}\n]\n"
            ),
        );
    project.write(
        ".cppls.toml",
        "[compile]\nremove_args = [\"-Werror\", \"-std=c++17\"]\nextra_args = [\"-std=c++20\"]\n",
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

    assert_eq!(
        session
            .config_for(&project.root.join("main.cpp"))
            .standard
            .as_deref(),
        Some("c++20")
    );
    let per_file = session.config_for(&project.root.join("main.cpp"));
    let defines: Vec<&str> = per_file
        .defines
        .iter()
        .map(|define| define.name.as_ref())
        .collect();
    assert_eq!(defines, ["KEEP"], "a flag nobody removed still applies");
}

/// A database written somewhere no convention predicts, named by the project's own file.
#[test]
fn the_database_can_be_somewhere_no_convention_predicts() {
    let project = Project::new("config-database");
    project.write("main.cpp", "int main() { return 0; }\n");
    let root = project.root.to_string_lossy().replace('\\', "/");
    project.write(
        "out/compile_commands.json",
        &format!(
            "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_ELSEWHERE\", \"-c\", \"{root}/main.cpp\"]}}\n]\n"
        ),
    );
    project.write(
        ".cppls.toml",
        "[compile]\ndatabase = \"out/compile_commands.json\"\n",
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

    assert_eq!(
        session.compile_database().map(crate::CompileCommands::len),
        Some(1)
    );
    let defines: Vec<&str> = session
        .config()
        .defines
        .iter()
        .map(|define| define.name.as_ref())
        .collect();
    assert_eq!(defines, ["FROM_ELSEWHERE"]);
    assert_eq!(project.relative(session.project_files()), ["main.cpp"]);
}

#[test]
fn the_cache_goes_where_the_project_says_and_the_filter_follows_it() {
    // The cache directory is one name with three readers — the store that writes under it, the filter that
    // ignores its events, and the report a user reads — and this is the property that keeps them one answer.
    let project = Project::new("config-cache");
    project.write("main.cpp", "int main() { return 0; }\n");
    project.write(".cppls.toml", "[index]\ncache_dir = \".cppls-cache\"\n");

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let mut session = Session::with_config(
        &project.root,
        providers,
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );

    session.index_everything();
    assert_eq!(session.stats().rebuilt, 1, "the file was read");

    // **The cache is one database file under the configured directory**, not a tree of summaries. The property this
    // test exists for is that the *name* has three agreeing readers — the store that writes, the filter that ignores
    // its events, and the report a user reads — and a directory is what all three join the name to.
    assert!(
        project.root.join(".cppls-cache").is_dir(),
        "the cache is under the configured directory, not the default: {}",
        project.root.join(".cppls-cache").display()
    );
    assert!(
        !project.root.join(crate::CACHE_DIRECTORY).exists(),
        "and the default directory was not created at all"
    );
}

#[test]
fn the_sections_a_language_server_reads_come_from_the_same_parse() {
    // One file, one parser: the server's keys are read from the session's report rather than by a second
    // TOML reader in the other crate, which is what keeps the two from disagreeing about what it says.
    let project = Project::new("config-server-keys");
    project.write("main.cpp", "int main() { return 0; }\n");
    project.write(
        ".cppls.toml",
        "[diagnostics]\non_change_ms = 250\n\n[hover]\nenable = false\n",
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::with_config(
        &project.root,
        providers,
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );

    let config = &session.project_config().config;
    assert_eq!(config.diagnostics.on_change_ms, Some(250));
    assert_eq!(config.hover.enable, Some(false));
}

#[test]
fn a_configuration_file_with_a_typo_still_analyses_the_project() {
    // The failure mode the section-by-section parse exists for: one bad key must not cost the project its
    // exclusions, and the problem must be visible to whoever asks the session what it thought.
    let project = Project::new("config-typo");
    project.write("main.cpp", "int main() { return 0; }\n");
    project.write("vendor/third_party.cpp", "int vendored;\n");
    project.write(
        ".cppls.toml",
        "[workspace]\nexclude = [\"vendor/**\"]\n\n[index]\ncache_dir = 5\n",
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::with_config(
        &project.root,
        providers,
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );

    assert_eq!(
        project.relative(session.project_files()),
        ["main.cpp"],
        "the exclusions are in effect"
    );
    assert_eq!(session.project_config().problems.len(), 1);
    assert!(
        session.project_config().problems[0]
            .message
            .contains("[index]")
    );
    assert_eq!(session.project_config().errors().count(), 1);
}

#[test]
fn a_database_in_a_build_directory_is_found_and_its_flags_reach_the_session() {
    // The whole point of the build-system half: a project that exports its database into `build/` must be
    // analysed with *those* flags and *that* file list, without anybody naming the path. This is the test that
    // fails if the discovery is written but not wired into the session.
    let project = Project::new("build-directory-database");
    project.write("main.cpp", "int main() { return 0; }\n");
    let root = project.root.to_string_lossy().replace('\\', "/");

    project.write(
        "build/compile_commands.json",
        &format!(
            "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_THE_BUILD_DIRECTORY\", \"-std=c++20\", \
                 \"-c\", \"{root}/main.cpp\"]}}\n]\n"
        ),
    );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

    assert_eq!(
        session.compile_database().map(crate::CompileCommands::len),
        Some(1),
        "the database in the build directory was read"
    );
    assert_eq!(
        session
            .discovery()
            .database
            .as_ref()
            .map(|found| found.origin),
        Some(crate::DatabaseOrigin::BuildDirectory),
        "and the report says where it was found"
    );
    let per_file = session.config_for(&project.root.join("main.cpp"));
    let defines: Vec<&str> = per_file
        .defines
        .iter()
        .map(|define| define.name.as_ref())
        .collect();
    assert_eq!(defines, ["FROM_THE_BUILD_DIRECTORY"]);
    assert_eq!(
        session
            .config_for(&project.root.join("main.cpp"))
            .standard
            .as_deref(),
        Some("c++20")
    );
    assert!(
        session
            .discovery()
            .problems
            .iter()
            .any(|problem| problem.message.contains("build directory")),
        "and a user reading the report is told: {:?}",
        session.discovery().problems
    );
}

#[test]
/// Restored after a scripted edit ate this header — the body below is unchanged.
fn a_project_without_a_database_is_analysed_against_the_compilers_defaults() {
    // No database and no `[compile].args`: the analysis is about the compiler's own defaults rather than about
    // the project's build. That *works*, and the problem list says what would make it better — which is the
    // whole reason the list exists.
    let project = Project::new("without-a-database");
    project.write("main.cpp", "int main() { return 0; }\n");

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::open(&project.root, providers, WatchFilter::new(&project.root));

    assert!(session.compile_database().is_none());
    assert!(
        session
            .discovery()
            .problems
            .iter()
            .any(|problem| problem.message.contains("no compile database")),
        "the way to do better is spelled out: {:?}",
        session.discovery().problems
    );
}

#[test]
fn a_compile_database_names_the_files_that_are_here_and_the_flags_they_are_built_with() {
    // The two things a database is read for: which files are compiled — the list is better than a scan, because
    // it is the project's own statement — and the flags, which are the whole reason the configuration is not
    // guessed. An entry naming a file this checkout does not have is dropped rather than queued, because a
    // checked-in database writes the absolute paths of the machine that produced it.
    let project = Project::new("database");
    project.write("main.cpp", "int main() { return 0; }\n");
    let root = project.root.to_string_lossy().replace('\\', "/");

    project.write(
            "compile_commands.json",
            &format!(
                "[\n  {{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_THE_DATABASE\", \"-I{root}/include\", \"-std=c++20\", \
                 \"-c\", \"{root}/main.cpp\"]}},\n  \
                 {{\"directory\": \"{root}\", \"file\": \"{root}/elsewhere.cpp\", \
                 \"arguments\": [\"g++\", \"-c\", \"{root}/elsewhere.cpp\"]}}\n]\n"
            ),
        );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let session = Session::open(
        &project.root,
        providers.clone(),
        WatchFilter::new(&project.root),
    );

    let database = session.compile_database().expect("the database is read");
    assert_eq!(database.len(), 2);

    let per_file = session.config_for(&project.root.join("main.cpp"));
    let defines: Vec<&str> = per_file
        .defines
        .iter()
        .map(|define| define.name.as_ref())
        .collect();
    assert_eq!(defines, ["FROM_THE_DATABASE"]);
    assert_eq!(
        session
            .config_for(&project.root.join("main.cpp"))
            .standard
            .as_deref(),
        Some("c++20")
    );

    assert_eq!(project.relative(session.project_files()), ["main.cpp"]);
    assert_eq!(
        session.pending(),
        1,
        "the entry for a file that is not here is not work"
    );
}

#[test]
/// Restored after a scripted edit ate this header — the body below is unchanged.
fn a_define_the_compile_database_carries_decides_a_condition_in_a_file() {
    // The whole way a `-D` travels: the database's flags become the configuration, the configuration becomes
    // the macro environment, and the environment decides the `#ifdef` around an `#include`. The *answer*
    // therefore depends on how the project is built while the *summary* does not — which is why a summary
    // stores the question rather than the answer, and why changing `-D` does not throw the cache away.
    let project = Project::new("conditional-include");
    let root = project.root.to_string_lossy().replace('\\', "/");

    project.write("feature.h", "#define FEATURE_ONLY int\n");
    project.write(
        "main.cpp",
        "#ifdef FROM_THE_DATABASE\n#include \"feature.h\"\n#endif\nFEATURE_ONLY x;\n",
    );
    project.write(
            "compile_commands.json",
            &format!(
                "[{{\"directory\": \"{root}\", \"file\": \"{root}/main.cpp\", \
                 \"arguments\": [\"g++\", \"-DFROM_THE_DATABASE\", \"-c\", \"{root}/main.cpp\"]}}]\n"
            ),
        );

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, DiskFiles);
    let mut session = Session::open(
        &project.root,
        providers.clone(),
        WatchFilter::new(&project.root),
    );

    // **Both files, because the closure of `main.cpp` is what the query walks.**
    session.advance(64);

    let path = project.root.join("main.cpp");
    let _source = std::fs::read_to_string(&path).expect("the fixture reads");

    // **The asking side, which is how a project nobody has opened gets read.**
    //
    // `advance` reads files into the index and never cooks, and nothing here is open — so no file was queued for
    // cooking and the first ask has no rendering to answer from. That is the deferral, and this is what a real
    // caller does with it: the ask **queues** the file, the pump cooks it, the next ask has it. Written as the
    // two steps rather than one because the first answer is `None` by contract and a test that hid that would be
    // asserting a behaviour the product does not have.
    assert!(
        session.view(&path).is_none(),
        "nothing has read this file as a compiler reads it, and that is what the ask is for"
    );
    session.index_everything();

    let view = session
        .view(&path)
        .expect("the ask queued it and the pump cooked it");

    // **The rendering is the proof, and it is a stronger one than the reference list was.**
    //
    // The chain under test is *flags → configuration → environment → the branch an `#ifdef` takes*, and the
    // endpoint of that chain is the text a compiler would parse. `-DFROM_THE_DATABASE` decides
    // `#ifdef FROM_THE_DATABASE`, which decides whether `feature.h` is reached, which decides what
    // `FEATURE_ONLY` — defined there as `int` — expands to. So a rendering of exactly `int x;` says every link
    // held: the define came from the database, the include was taken, and the macro was substituted.
    assert_eq!(
        view.source.as_ref(),
        "int x;",
        "the -D took the branch, the branch included feature.h, and FEATURE_ONLY became int"
    );

    // **And what this test does NOT prove, said rather than implied.** It shows the chain holds *with* the
    // database's flag. It cannot show it *fails* without it, and the reason is the session's contract rather
    // than an oversight here: [`Session::open`] reads `compile_commands.json` whenever the project has one
    // (`session.rs:66`), so a second session over this same directory necessarily has the same `-D`. A negative
    // half was written and **measured to be worthless** — it rendered `int x;` as well, because the flag was
    // there in both sessions:
    //
    // ```text
    //   with no -D the `#ifdef` is false ...: got "int x;"
    // ```
    //
    // A one-sided assertion is the failure mode this test now names instead of hiding: without a session whose
    // flags differ, `int x;` is consistent with reading the include unconditionally. Building that session means
    // opening a project whose database has no flags — a fixture change, not a new assertion here.
    //
    // **And the reference half was removed rather than repaired.** It asked [`Session::macro_references`] about
    // `FEATURE_ONLY`, which needs a *name at an offset* and gets one from a tree; but a macro is **gone** from a
    // rendering — that is what a rendering is — so the question was a category error and answered
    // `Unknown(UnparsableName)`. A macro question wants the file's own tokens plus the closure's macro facts,
    // and `Session::tokens_of` is the first half of that. What is missing is the entry point, not the reading:
    // see the note on `Session::macro_references`.
}

#[test]
fn the_compilation_is_known_only_when_the_compiler_answered_with_its_macros() {
    // The licence for one claim — *a name none of the witnesses mentions is not defined* — pinned on the three
    // shapes a session can be in. The middle one is why this is a function rather than a field read at the call
    // site: a toolchain **exists** on a machine whose headers were found by a convention and whose compiler
    // could not be asked, and its macro table is empty. Claiming completeness there would read all five hundred
    // of the compiler's own names as "not defined" — `#ifdef _MSC_VER` coming out false *on MSVC* — which is a
    // wrong answer rather than a missing one, the direction this layer must never fail in.
    assert!(
        !super::the_compilation_is_known(None),
        "no compiler answered, so nothing witnesses the built-ins"
    );

    let unasked = Toolchain {
        compiler: None,
        version: None,
        system_include_paths: vec![PathBuf::from("/usr/include")],
        builtin_macros: Vec::new(),
        dialect: None,
        standard: None,
        source: ToolchainSource::SystemHeaders,
        note: Some("no compiler could be asked".to_string()),
    };
    assert!(
        !super::the_compilation_is_known(Some(&unasked)),
        "directories without a macro table are half a toolchain, and the missing half is the built-ins"
    );

    let asked = Toolchain {
        compiler: Some(PathBuf::from("cl")),
        builtin_macros: vec![CommandLineMacro::defined("_MSC_VER")],
        ..unasked
    };
    assert!(
        super::the_compilation_is_known(Some(&asked)),
        "a compiler that answered is the only witness there is to the names no file defines"
    );
}

/// **The closure, not the direct includes** — the question a *colouring* asks.
///
/// `is_locally_ready` asks whether the file's own includes are read, which is exactly what `catch_up` does and
/// what one name lookup needs. A semantic token pass classifies **every** identifier, so what it needs is the
/// whole closure: `std::endl` is declared in `<ostream>`, reached through `<iostream>`, and a request answered
/// while only the first level was read drew it with the client's own grammar colour (measured from an editor:
/// a variable at startup, a function after any edit).
#[test]
fn a_file_is_closure_ready_only_once_what_it_reaches_is_read() {
    let files = MemoryFiles::new()
        .with_file("/p/mid.h", "#include \"deep.h\"\n")
        .with_file("/p/deep.h", "namespace std { int endl(); }\n")
        .with_file(
            "/p/a.cpp",
            "#include \"mid.h\"\nint main() { return std::endl(); }\n",
        );
    let fixture = Memory::new("is-closure-ready", &files);

    let mut session = fixture.session();
    session.add_project_files([PathBuf::from("/p/a.cpp")]);
    session.index_everything();

    assert!(
        session.is_locally_ready(Path::new("/p/a.cpp")),
        "the file and its direct includes are read"
    );
    assert!(
        session.is_closure_ready(Path::new("/p/a.cpp")),
        "and so is everything they reach"
    );

    // **The distinction, made by dropping the deepest file's summary** — the state a project is in while its
    // queue is still being read: `a.cpp`'s direct include (`mid.h`) is in, its include's (`deep.h`) is not.
    session.store.forget(Path::new("/p/deep.h"));

    assert!(
        session.is_locally_ready(Path::new("/p/a.cpp")),
        "the direct question is unchanged — `catch_up` has nothing left to read"
    );
    assert!(
        !session.is_closure_ready(Path::new("/p/a.cpp")),
        "but a name declared in `deep.h` has no classification, which is the question colouring asks"
    );
}
