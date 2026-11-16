//! **Cooked** — what a cooked reading answers: declarations a macro unlocks, headers a request names

use crate::file::paths::MemoryFiles;
use crate::symbol::Known;
use std::path::Path;

use super::*;

#[test]
pub(crate) fn a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file() {
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
pub(crate) fn a_header_the_user_never_opened_is_cooked_when_a_request_names_it() {
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
pub(crate) fn a_file_that_does_not_balance_its_braces_is_named_and_still_read() {
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
pub(crate) fn a_single_bracket_attribute_no_longer_props_the_program_open() {
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
pub(crate) fn a_file_that_does_not_balance_costs_nothing_but_a_name() {
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
pub(crate) fn a_program_whose_files_keep_their_scopes_quarantines_nothing() {
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
pub(crate) fn a_unit_read_reads_the_whole_program_once() {
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
pub(crate) fn a_program_is_read_when_the_reader_stops_editing() {
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
    std::thread::sleep(
        crate::session::UNTIL_A_UNIT_MAY_BE_READ + std::time::Duration::from_millis(100),
    );
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

/// **A program of any size is read.** This used to be the other half of the schedule: a program over eight megabytes
/// of source was refused, because a unit read is linear in its program and its preparing half held the read lock.
/// Measured with a counter that counts frees as well as allocations, the size was never the problem — a 159-file
/// program is 5.09 s and ~28 MB of live bytes — and the ceiling only made a reader's file keep the per-file reading.
/// The fixture is the same shape as the test above, with one large comment-only header in the closure.
///
/// # Why the assertion is about the read rather than about the queue
///
/// Because **the drain may have already done it**. The fixture's index takes longer than
/// [`UNTIL_A_UNIT_MAY_BE_READ`] — nine megabytes of comment is nine megabytes to lex — so the pump's own `advance`
/// reaches the pause inside `index_everything`, reads the program, and marks the root read. That is the right
/// outcome, and it made this test fail for the wrong reason: it asserted the *schedule* named the file, which is
/// only true while nothing has read it. So the question is asked in both of its shapes — the root is either already
/// read, or it is the next one to read — and "it was refused for being large" is the one answer both halves rule out.
#[test]
pub(crate) fn a_program_of_any_size_is_read() {
    // Bigger than the old ceiling, and sized from the text rather than from a constant that no longer exists.
    let line = "// padding padding padding padding\n";
    let big = line.repeat((9 * 1024 * 1024) / line.len() + 1);
    assert!(
        big.len() > 8 * 1024 * 1024,
        "larger than the ceiling that was: {} bytes",
        big.len()
    );

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
    let fixture = Memory::new("a-program-of-any-size", &files);
    let mut session = fixture.session();

    session.did_open("/p/main.cpp", main);
    session.index_everything();
    session.pretend_a_pause(
        crate::session::UNTIL_A_UNIT_MAY_BE_READ + std::time::Duration::from_millis(1),
    );

    let root = crate::session::queue_key(Path::new("/p/main.cpp"));
    match session.the_program_to_read() {
        // The drain has not reached it yet: the schedule still owes this program, whatever its closure weighs.
        Some(next) => assert_eq!(next.as_path(), Path::new("/p/main.cpp")),
        // The drain has read it already — which is the same claim about size, made by the read itself.
        None => assert!(
            session.units_read.contains(&root),
            "the program is neither read nor owed: the file kept its per-file reading, which is what the old \
             size ceiling did"
        ),
    }
}
