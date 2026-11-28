//! Completion, member lists, completion resolve and the edits they carry.

use super::fixtures::*;
use super::support::*;

/// **The other query, over the wire: after a `.`, what the object's type has.**
///
/// `w.` asks for the members of `w`'s type, and the type is written `one::Widget` — a qualified spelling the lookup
/// has to follow before the class body can be read at all. The dependency on the second reading here is **not** the
/// point of the test (the raw reading resolves `one::Widget` too, since a namespace-opening macro is a shape the
/// rules already handle — see the fixture's note); what it covers is the dispatch itself: a cursor after a dot goes
/// to the member query, its answer arrives as members with the range a client replaces, and the file-scope names
/// are not offered beside them.
#[test]
fn a_members_members_are_offered_over_the_wire() {
    let project = Project::new("completion-member");
    project.write("namespace.h", NAMESPACE_H);
    project.write("widget.h", NAMESPACED_WIDGET_H);
    project.write("main.cpp", MEMBER_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MEMBER_CPP }
        }),
    );

    // Line 3 is `    w.` — the cursor is just past the dot; asked again until the list holds `size`, for the reason
    // the qualified test gives.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 3, "character": 6 },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("size")))
        },
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
    let size = items
        .iter()
        .find(|item| item["label"] == json!("size"))
        .unwrap_or_else(|| panic!("`Widget` has a `size` member: {answer}"));
    assert_eq!(
        size["kind"],
        json!(5),
        "a **field** rather than a variable, because it is reached through an object — the completion layer \
         distinguishes the two positions, and a client draws them with different icons: {size}"
    );
    // **The short type line is in the list.** It tells `items` from `size` without opening anything, and a
    // macro-declared struct has no other place to show it. The full declaration is still fetched by
    // `completionItem/resolve` for the one item a user is looking at; the list carries only the one-line type.
    assert_eq!(
        size["detail"],
        json!("int"),
        "the member's type belongs beside its name: {size}"
    );
    assert_eq!(
        size["labelDetails"]["detail"],
        json!("  int"),
        "a 3.17 client draws this beside the label without resolving: {size}"
    );
    let resolved = server.request(4, "completionItem/resolve", size.clone());
    assert_eq!(
        resolved["result"]["detail"],
        json!("int"),
        "and resolve supplies the type the declaration was written with: {resolved}"
    );
    assert_eq!(
        resolved["result"]["labelDetails"]["detail"],
        json!("  int"),
        "as the label's second half too, which is where 3.17 draws it: {resolved}"
    );
    assert_eq!(
        size["textEdit"]["range"]["start"],
        json!({ "line": 3, "character": 6 }),
        "the edit is an empty range just past the dot: {size}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A `.` on a `std::string` offers the standard library's members, once each.**
///
/// The case a user reported as "补全依然存在明显错误": the popup after `full.` was a list of file-scope names —
/// `printf`, `full`, `myName`, `sum`, `main` — plus keywords, which is what a client is shown when the member
/// question has no answer. Two separate faults can produce that shape, and this test pins both:
///
/// * **the members are found at all**, through `#include <string>` and the type's declaration in a header the
///   session had to index — the whole chain has to work for `std::string` to resolve;
/// * **each spelling appears once.** MSVC's `basic_string` declares `replace` eleven times and `insert` nine, and
///   the list used to carry every one of them: 119 rows of which 64 shared a label with another row. A list is
///   something a reader picks from, so eleven rows reading `replace` are one choice offered eleven times — and a
///   client that lays out 119 rows with duplicated labels is what "the popup looks broken" means.
///
/// It is an end-to-end test rather than a unit one because the failure was a *chain*: the fix could be right in the
/// completion layer and the answer still wrong if the index never resolved `<string>`.
///
/// # `#[ignore]`, and why it is not a disabled test
///
/// It is **too slow for the debug profile**: the fixture's one `#include <string>` is a closure of thousands of
/// MSVC headers, which the index reads before it can answer, and a debug build does that in minutes rather than in
/// the sixteen seconds a release build takes. The harness's own deadline is therefore unreachable in a debug run
/// — and a test that fails for a *build profile* teaches nothing about the code, which is the one thing a test
/// must not do.
///
/// It runs in release, and that is how it was written and verified:
///
/// ```text
/// cargo test --release -p cpp_ls --test suite handshake -- --ignored a_standard_library
/// ```
///
/// What covers the same ground in a debug run: `cpp_code_analysis/tests/completion.rs`'s
/// `a_member_position_offers_the_members_with_their_distinctions` (the member list and its kinds, from a fixture
/// with no standard library in it) and the probe, which is the instrument this was measured with.
#[test]
#[ignore = "needs the release profile: the standard library closure takes minutes to index in debug"]
fn a_standard_library_member_list_has_each_spelling_once() {
    let project = Project::new("completion-std-string");
    project.write("main.cpp", STD_STRING_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": STD_STRING_CPP }
        }),
    );

    // **The position of the operator, computed from the fixture** rather than counted by hand.
    //
    // This test was written with the position hard-coded twice and wrong twice, and the failure is invisible: an
    // off-by-one lands on the newline (still a name position, so the answer looks like "the members were not
    // found") or on the closing brace (still an answer about a position nobody is looking at). A test whose
    // subject is *where* the cursor is cannot afford to state the where by hand.
    let (line, character) = position_of(STD_STRING_CPP, "full.");
    assert_eq!(
        (line, character),
        (3, 9),
        "the operator ends the fourth line and the cursor is just past it — written out here once, so that a \
         fixture edited above this line fails loudly instead of silently asking about the wrong place"
    );

    // Asked until the list holds `append`, because the standard library's closure is thousands of files and the
    // index is still reading when the first request arrives.
    let answer = server.ask_until_it_within(
        100,
        STD_LIBRARY_TIMEOUT,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": line, "character": character },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("append")))
        },
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));

    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();

    assert!(
        labels.contains(&"append") && labels.contains(&"size") && labels.contains(&"substr"),
        "the members of the standard library's string: {labels:?}"
    );
    assert!(
        !labels.contains(&"printf") && !labels.contains(&"main"),
        "and not the names in scope, which cannot follow the operator: {labels:?}"
    );

    let mut unique = labels.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        labels.len(),
        "every spelling appears once — {} of {} rows are repeats: {:?}",
        labels.len() - unique.len(),
        labels.len(),
        labels
            .iter()
            .filter(|label| labels.iter().filter(|other| other == label).count() > 1)
            .collect::<Vec<_>>()
    );

    // Each row carries an **edit that replaces what was typed**, so that the choice is inserted rather than
    // appended to the half-written name — the same contract the `Widget` case asserts, on a type that came from a
    // header rather than from the buffer.
    let append = items
        .iter()
        .find(|item| item["label"] == json!("append"))
        .expect("asserted above");
    assert_eq!(
        append["textEdit"]["range"]["start"],
        json!({ "line": line, "character": character }),
        "just past the dot: {append}"
    );
    assert_eq!(append["textEdit"]["newText"], json!("append"), "{append}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A member list after `.` edits at the cursor, not at the next line.**
///
/// The members were found — `capacity`, `items`, `size` — but their replace range was mapped from the rendering with
/// `file_offset_of`, which answers where an offset *starts*. An empty range at a token boundary came back at the start
/// of the next line, the client discarded every item because the edit was somewhere else, and the popup never appeared.
/// Measured on neovim's `channel.c` at `ids.` after `kvec_t(int64_t) ids`.
#[test]
fn a_member_completion_after_a_dot_edits_at_the_cursor() {
    let source = "\
#define KV(T) struct { T *items; int size; }
void f() {
  KV(int) ids;
  ids.
}
";
    let project = Project::new("member-edit-range");
    project.write("a.cpp", source);
    let uri = uri_of(&project.root().join("a.cpp"));
    let mut server = Server::start(project.root());
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {},
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );

    let response = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": 3, "character": 6 },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == "size"))
        },
    );
    let items = response["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a member list: {response}"));
    let size = items
        .iter()
        .find(|item| item["label"] == "size")
        .unwrap_or_else(|| panic!("the size member: {response}"));
    assert!(
        size["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("int")),
        "the member's type belongs beside its name: {size}"
    );
    for item in items {
        let range = &item["textEdit"]["range"];
        assert_eq!(
            range["start"],
            json!({ "line": 3, "character": 6 }),
            "{item}"
        );
        assert_eq!(range["end"], json!({ "line": 3, "character": 6 }), "{item}");
    }
}

/// **An edit to an included header is visible to the completion that follows it.**
///
/// This is the shape a user reported and diagnosed as "文件的 didchange 尚未处理完毕": an edit reaches the analysis in
/// two steps that are not the same step. `didChange` replaces the **text** immediately — the scope tree is rebuilt
/// from it on every parse — and *drops the file's summary*, which is rebuilt later, when the index pump reaches that
/// file. A request answered inside that window sees the new text with the **old facts**.
///
/// What it covers is the *atomicity of the pair*: the change and the request arrive with nothing in between, and the
/// answer is about the changed text. What it does **not** discriminate is the fix itself — the class here is in the
/// buffer's own scope tree (a view parses the text it holds, includes and all), so the members are answerable from
/// the tree whether or not the summary was rebuilt. The falsification for
/// [`cpp_code_analysis::Session::catch_up`] is at the session level, where the staleness is visible and measured:
/// `cpp_code_analysis/tests/completion.rs::a_completion_after_an_edit_is_about_the_file_the_edit_wrote`, which
/// asserts that the stale reading is *wrong* and that `catch_up` is what makes it right.
///
/// # Why the change is to a header rather than to `main.cpp`
///
/// Because a change to the file being edited moves the cursor, and the request would then be about a position that
/// did not exist before. Rewriting `widget.h` leaves `w.` where it was.
#[test]
fn a_completion_sees_a_change_to_an_included_header() {
    let project = Project::new("completion-after-header-change");
    project.write("widget.h", FRESH_TYPE_H);
    project.write("main.cpp", FRESH_TYPE_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let header_uri = uri_of(&project.root().join("widget.h"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": FRESH_TYPE_CPP }
        }),
    );
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": header_uri, "languageId": "cpp", "version": 1, "text": FRESH_TYPE_H }
        }),
    );

    // Wait until the list is the members of the type **as it is now**, so that the assertion below is about the
    // change and not about the index being cold.
    let (line, character) = position_of(FRESH_TYPE_CPP, "w.");
    server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": line, "character": character },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("size")))
        },
    );

    // **The change**, and then the request with nothing in between.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": header_uri, "version": 2 },
            "contentChanges": [{ "text": "struct Widget { int size; int added_by_the_edit; };\n" }],
        }),
    );

    // **Asked once, with nothing in between**: the request a client sends in the same breath as the keystroke, and
    // the one that used to be answered from the header's *old* summary — with no members at all, or with the names
    // in scope instead.
    let answer = server.request(
        200,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": line, "character": character },
        }),
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));

    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();

    assert!(
        labels.contains(&"added_by_the_edit"),
        "the member the edit added, which only a rebuilt summary can express — asked once, right after the change, \
         with no retry to hide a stale answer: {labels:?}"
    );
    assert!(
        !labels.contains(&"main"),
        "and not the names in scope, which is what a stale summary produces: {labels:?}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The keystroke that asked for the completion is the one the answer is about.**
///
/// A client sends `didChange` and then the request, with nothing in between: neovim flushes its pending changes
/// before every request (`vim.lsp.Client:request` → `changetracking.flush`, "so that the server doesn't operate on
/// a stale state"). The notification only *queues* the edit, so everything rides on the request's barrier —
/// `record_request` (the queue's position when the request was dispatched) and `catch_up_upto` (wait until that
/// position is applied), which run before the handler's first line.
///
/// The assertion is the **replace range**: one character, columns 2..3. An answer computed from the text before
/// the keystroke has an empty range at the cursor and no prefix at all — which is the shape reported from an
/// editor as "typing `k` shows nothing".
#[test]
fn a_completion_sees_the_keystroke_that_asked_for_it() {
    // The blank line is two spaces, spelled inside a concatenation: a literal line of trailing spaces is removed by
    // the repository's whitespace hook, and the cursor position below depends on those two spaces.
    let source = concat!(
        "#define kvec_make(T) struct { T *items; int size; }\n",
        "void f() {\n",
        "  \n",
        "}\n",
    );
    let project = Project::new("completion-sees-its-own-keystroke");
    project.write("a.c", source);
    let uri = uri_of(&project.root().join("a.c"));

    let mut server = Server::start(project.root());
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {},
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": source }
        }),
    );

    let (line, character) = (2u32, 2u32);
    let ask = |id: i64| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            },
        })
    };
    server.ask_until_it(100, ask, |response| {
        response["result"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["label"] == json!("kvec_make")))
    });

    // **The keystroke, and the request with nothing in between.**
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": line, "character": character },
                    "end": { "line": line, "character": character },
                },
                "text": "k",
            }],
        }),
    );
    let answer = server.request(
        200,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character + 1 },
        }),
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
    let item = items
        .iter()
        .find(|item| item["label"] == json!("kvec_make"))
        .unwrap_or_else(|| {
            let labels: Vec<&str> = items.iter().filter_map(|item| item["label"].as_str()).collect();
            panic!("the macro the file declares, from a list about the text that has the `k` in it: {labels:?}")
        });
    let range = &item["textEdit"]["range"];
    assert_eq!(
        range["start"],
        json!({ "line": line, "character": character }),
        "the edit starts at the `k` the reader typed: {item}"
    );
    assert_eq!(
        range["end"],
        json!({ "line": line, "character": character + 1 }),
        "and covers exactly that one character — an empty range at the cursor is an answer from the old text: {item}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A `.` at the end of a body, on a `std::string` variable, offers the members.**
///
/// The third report of the same symptom — "No suggestions" under `full2.` — and the one that shows how narrow the
/// failure is: the same file answered correctly when the line below the cursor was something else, and answered
/// nothing when the operator was the **last line of the body**, with `}` after it. That is the state a reader is in
/// constantly (type the line, press the key, and the closing brace is what follows), so it is the state this test
/// holds fixed.
///
/// The answers that are *not* acceptable here, and why each of them used to happen:
///
/// * an **empty list** — the member query could not work out the type of the object;
/// * the **names in scope** (`myName`, `full2`, `main`, the keywords) — which is what a declined member query falls
///   through to, and a list of things that cannot follow a `.`;
/// * a **different list at the two positions a caret after the operator can be reported at** — measured on a live
///   server: the request one column early was answered from the tree, found the name `full2`, and offered the names
///   in scope.
///
/// **Release only**, for the reason `a_standard_library_member_list_has_each_spelling_once` gives: the answer needs
/// `std::string`, and *this project has no sources of its own for the pump to seed from* — the client's buffer is
/// the only file — so the assertion can only be met once the pump has read the 156 files of MSVC's `<iostream>`/
/// `<string>` closure. Measured: **23.8 s in release**, past the debug profile's deadline. The debug-equivalent is
/// `cpp_code_analysis/tests/completion.rs`'s
/// `a_member_access_on_the_last_line_of_a_body_offers_its_members_at_either_caret`, which holds the same shape with
/// a header of the project's own.
///
/// ```text
/// cargo test --release -p cpp_ls --test suite handshake -- --ignored a_member_access_on_the_last_line
/// ```
#[test]
#[ignore = "needs the release profile: the answer comes from the standard library's include closure (156 files)"]
fn a_member_access_on_the_last_line_of_a_body_offers_the_members() {
    let project = Project::new("completion-last-line");
    project.write("main.cpp", LAST_LINE_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": LAST_LINE_CPP }
        }),
    );

    // **The two characters a caret after a typed `.` can be reported at.**
    //
    // A caret is drawn *between* two characters, and `position_of` returns the offset **past** `full2.` — which is
    // where the caret sits once the operator has been typed. The other position a reader can mean is one before it:
    // the newline's own column (`full2.|`). Both mean "after the operator" to a reader and both must answer the same
    // list, which is exactly the pair a live server disagreed about — a request one column early was answered from
    // the tree, found the name `full2` and offered the names in scope instead of the members.
    //
    // The characters **at or before** the operator's own column are deliberately not asked about here: there the
    // question is about the name `full2`, which is a different question with a different (and correct) answer.
    // Getting this arithmetic wrong is how three separate reports of "the completion is wrong here" turned out to be
    // about a cursor one column away from the operator — see `position_of` and the note on
    // `log_a_member_that_produced_no_members`.
    let (line, ends_at) = position_of(LAST_LINE_CPP, "full2.");

    for character in [ends_at, ends_at - 1] {
        let answer = server.ask_until_it_within(
            100,
            STD_LIBRARY_TIMEOUT,
            |id| {
                json!({
                    "id": id,
                    "method": "textDocument/completion",
                    "params": {
                        "textDocument": { "uri": main_uri },
                        "position": { "line": line, "character": character },
                    },
                })
            },
            |response| {
                response["result"]["items"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["label"] == json!("append")))
            },
        );

        let items = answer["result"]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();

        assert!(
            labels.contains(&"append") && labels.contains(&"size"),
            "the members of the standard library's string, at line {line} character {character}: {labels:?}"
        );
        assert!(
            !labels.contains(&"full2") && !labels.contains(&"myName"),
            "and not the names in scope, at line {line} character {character}: {labels:?}"
        );
    }

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A completion asked for right after a change sees the change.**
///
/// A client sends `didChange` and then, without waiting for anything, asks for a completion at a position that only
/// exists in the **new** text — which is what typing is: the request follows the keystroke that caused it. The
/// server queues notifications so that its message loop never waits for a parse, and the queue must not be visible
/// to a request: while it was, the analysis still held older text, the cursor was past the end of that line, no
/// node was there, and the answer was `null` — a completion that simply does not appear.
///
/// The burst is what makes it a test rather than a coin toss: with one change the queue may well be applied before
/// the request is served (measured: the same assertion failed on 1 run in 4), so the fixture sends a **stream** of
/// changes the way a client that is typing, pasting or formatting does, and only the last of them has the `w.` the
/// request is about. Any staleness at all therefore fails: an older text has that line blank.
#[test]
fn a_completion_right_after_a_change_sees_the_change() {
    /// Long enough that applying one change is real work — the queue has to be behind for the race to exist.
    const LINES: usize = 1200;
    /// The burst. Each round is a different text, so each is a change a client could have sent.
    const ROUNDS: usize = 24;

    let text = |round: usize, dot: bool| {
        let mut text =
            String::from("struct Widget { int size; int weight; };\nvoid f() {\n    Widget w;\n");
        for index in 0..LINES {
            text.push_str(&format!("    w.size = {};\n", index + round));
        }
        text.push_str(if dot { "    w.\n}\n" } else { "    \n}\n" });
        text
    };

    let project = Project::new("completion-after-change");
    project.write("main.cpp", &text(0, false));

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": text(0, false) }
        }),
    );

    // The burst, and then **immediately** the request: no sleep, no retry, no second request. A client that typed a
    // dot sends its edits and then asks, in this order.
    let sent = Instant::now();
    for round in 1..=ROUNDS {
        server.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": round + 1 },
                "contentChanges": [{ "text": text(round, round == ROUNDS) }],
            }),
        );
    }
    let burst_written = Instant::now();

    let answer = server.request(
        100,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3 + LINES, "character": 6 },
        }),
    );
    let served = Instant::now();

    // Reported rather than asserted: how long the burst costs the request that follows it is a measurement, and it
    // moves with the machine. What *is* asserted is the count of applications, in `context::update_queue`'s own
    // tests — a burst of changes for one document is one piece of work, so this request waited for one.
    println!(
        "a {ROUNDS}-change burst sent in {:?}, then a completion answered {:?} later",
        burst_written.elapsed(),
        served - sent
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("`w.` in the changed text must complete, got {answer}"));

    assert!(
        items.iter().any(|item| item["label"] == json!("weight")),
        "and the list is the *changed* class's members — `weight` is the member the change added: {answer}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

#[test]
fn an_incremental_edit_is_visible_to_the_next_completion() {
    let project = Project::new("incremental-completion");
    let source = "struct Widget { int size; };\nvoid f() {\n    Widget w;\n    w.\n}\n";
    project.write("main.cpp", source);
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.cpp"));
    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {},
        }),
    );
    assert_eq!(
        initialized["result"]["capabilities"]["textDocumentSync"]["change"],
        json!(2)
    );
    // **The save-time formatting request is advertised**, because it is implemented: a client that asks before saving
    // gets the formatter's edits, and a capability that answers `None` means no compliant client ever asks
    // (docs/worklog.md P2-1).
    assert_eq!(
        initialized["result"]["capabilities"]["textDocumentSync"]["willSaveWaitUntil"],
        json!(true),
        "the implemented save-time request is in the capability: {initialized}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );
    let (line, character) = position_of(source, "w.");
    server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("size")))
        },
    );

    let (edit_line, edit_character) = position_of(source, "int size;");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": edit_line, "character": edit_character },
                    "end": { "line": edit_line, "character": edit_character }
                },
                "text": " int added;"
            }]
        }),
    );
    let answer = server.request(
        200,
        "textDocument/completion",
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
    );
    assert!(
        answer["result"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["label"] == json!("added"))),
        "the incremental edit must be visible immediately: {answer}"
    );
}

#[test]
fn a_module_is_read_in_the_background_for_completion() {
    let project = Project::new("background-module");
    let source = "import report;\nvoid f() { Report r; r.\n}\n";
    project.write("main.cpp", source);
    project.write(
        "report.cppm",
        "export module report;\nexport struct Report { int module_member; };\n",
    );
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "diagnostic": {} } }
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );
    let (line, character) = position_of(source, "r.");
    server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == "module_member"))
        },
    );
}

/// **`completionItem/resolve` fetches the documentation for the one item a client is showing.**
///
/// The list itself carries no comments — that is the point of the round trip, and why the list stays cheap for a
/// hundred names — so this test asks the two questions a client asks, in order: complete, then resolve the item the
/// user highlighted. What comes back is the declaration's own comment, rendered by the same function the hover and
/// the signature help use.
#[ignore]
#[test]
fn a_completion_item_resolves_to_its_declarations_documentation() {
    let project = Project::new("completion-resolve");
    project.write("main.cpp", RESOLVE_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": RESOLVE_CPP }
        }),
    );

    let line = RESOLVE_CPP
        .lines()
        .position(|text| text.trim() == "w.")
        .expect("the fixture writes the access") as u64;
    let character = RESOLVE_CPP
        .lines()
        .nth(line as usize)
        .and_then(|text| text.find("w."))
        .expect("the fixture writes the access")
        + 2;

    let listed = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": line, "character": character },
                },
            })
        },
        |answer| {
            answer["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("size")))
        },
    );

    let item = listed["result"]["items"]
        .as_array()
        .expect("an item list")
        .iter()
        .find(|item| item["label"] == json!("size"))
        .expect("the member is offered")
        .clone();

    // The item carries **where the declaration is** — the file and the name's offset — which is what a resolve is
    // asked with. A client echoes `data` back untouched; this test does the same.
    assert!(
        item["data"]["file"].is_string() && item["data"]["offset"].is_number(),
        "the item carries its declaration's identity: {item}"
    );
    assert!(
        item["documentation"].is_null(),
        "the list itself carries no comments: {item}"
    );

    let resolved = server.ask_until_it(
        200,
        |id| {
            json!({
                "id": id,
                "method": "completionItem/resolve",
                "params": item.clone(),
            })
        },
        |answer| !answer["result"]["documentation"].is_null(),
    );

    let documentation = resolved["result"]["documentation"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("markdown documentation was expected, got {resolved}"));
    assert!(
        documentation.contains("The widget's size in cells."),
        "the member's own comment: {documentation}"
    );
    assert_eq!(
        resolved["result"]["label"], "size",
        "and the item is the one that was asked about: {resolved}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A hover on the last member of a chain reads the chain, not its head.**
///
/// `win->w_buffer->fnum`: the object of the access is `win->w_buffer`, and the *first* byte of that text is the `w`
/// of `win` — a node whose type is `win_T*`. The hover asked `members_of("win_T*")` for `fnum`, which is not a member
/// of `win_T` at all, and said so:
///
/// ```text
/// `win_T*` has no member `fnum`
///
/// `win->w_buffer` is a `win_T*`.
/// ```
///
/// Measured on neovim's `src/nvim/arglist.c` (`win->w_buffer->b_fnum`; `struct file_buffer` declares the field the
/// macro `#define b_fnum handle` renames). The last byte of the object is past the base name, so the node it sits in
/// is the chain itself and its type is `buf_T*` — which is the object's real type and what the answer has to use.
#[test]
fn a_hover_on_the_last_member_of_a_chain_reads_the_chain() {
    const FIXTURE: &str = "\
typedef struct { int fnum; } buf_T;
typedef struct { buf_T *w_buffer; int other; } win_T;
int f(win_T *win) { return win->w_buffer->fnum; }
";
    let project = Project::new("hover-chain");
    project.write("main.c", FIXTURE);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.c"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": FIXTURE }
        }),
    );

    // **Inside the member's name**, not on the `->` in front of it: a hover on the operator is a question about the
    // operator.
    let line = line_of(FIXTURE, "->w_buffer->fnum");
    let character = FIXTURE
        .lines()
        .nth(line as usize)
        .and_then(|text| text.find("fnum"))
        .expect("the fixture writes it")
        + 2;
    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            },
        })
    });
    let text = answer["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {answer}"));
    assert!(
        !text.contains("has no member"),
        "`fnum` is a member of the chain's type, so nothing may say otherwise: {text}"
    );
    assert!(
        text.contains("int"),
        "and the popup says what the member is: {text}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A hover after an arrow reads the pointee's members**, the same reader the completion uses.
///
/// `h->load()` on a `Handle<Config>` is `Config::load`: the handle declares `T* operator->()`. The hover used to ask
/// `members_of` about the *written* type, so it looked `load` up in `Handle` — measured through the server on a real
/// project, "`std::shared_ptr<std::string>` has no member `c_str`" while the completion at the same cursor offered 187
/// members of `std::string`. `h.get()` must still answer with the handle's own member, which is why the pointee is
/// asked first and the handle second.
#[test]
fn a_hover_after_an_arrow_reads_the_pointees_members() {
    const FIXTURE: &str = "\
struct Config { void load(); };
template <class T>
struct Handle {
    T* operator->();
    T* get() const;
    void reset();
};
void f() {
    Handle<Config> h;
    h->load();
    h.get();
}
";
    let project = Project::new("hover_arrow");
    project.write("main.cpp", FIXTURE);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": FIXTURE }
        }),
    );

    let pointee = hover_at(&mut server, 100, &main_uri, FIXTURE, "h->load", 3);
    let text = pointee["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {pointee}"));
    assert!(
        text.contains("Config") && text.contains("load"),
        "`h->load` is `Config::load`, through the handle's `operator->`: {text}"
    );

    let handle = hover_at(&mut server, 200, &main_uri, FIXTURE, "h.get", 2);
    let text = handle["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {handle}"));
    assert!(
        text.contains("get") && text.contains("Handle"),
        "`h.get` is the handle's own member, asked second: {text}"
    );

    server.request(300, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A cursor inside a macro's argument list is still a cursor in the code.**
///
/// A view is of the **rendering** — the file with its macros replaced — so a position a client sends has to be
/// translated into the reading before anything is resolved. A macro invocation is where that translation is hardest
/// and where getting it wrong is invisible: `PICK(1, bet.)` becomes something else entirely in the rendering, and a
/// mapping that dropped the cursor at the invocation's start rather than inside the argument would still produce a
/// completion list — of the wrong scope, at the wrong place, with no error anywhere to say so.
///
/// The criterion is that the **argument's own type** is what is offered. `bet` is a `beta` and the file also holds
/// an `alpha`, so a list holding `one` is a list answering about some other expression.
#[test]
fn a_completion_inside_a_macros_argument_is_about_that_argument() {
    let project = Project::new("completion-in-a-macro-argument");
    project.write("main.cpp", MACRO_ARGUMENT_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "completion": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MACRO_ARGUMENT_CPP }
        }),
    );

    // The cursor is right after `bet.` inside `PICK(1, bet.)` — a member access whose subject is a macro argument.
    // Line 8 is `    return PICK(1, bet.);` and column 23 is the byte after the dot: the first version of this test
    // asked at line 9, which is the closing brace, and the server answered a name completion for `}` — a correct
    // answer to a question about the wrong place, which is the failure mode this test exists to catch and which it
    // caught in itself first.
    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 8, "character": 23 },
            },
        })
    });

    let items = answer["result"]["items"]
        .as_array()
        .or_else(|| answer["result"].as_array())
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();

    assert!(
        labels.contains(&"gamma"),
        "the members of the argument's own type are offered: {labels:?}"
    );
    assert!(
        !labels.contains(&"one"),
        "and not the members of the other type in the same file: {labels:?}"
    );
}
/// **A hover on a member resolves through the object's type, and says so when it cannot.**
///
/// The shape from a real editor session: `auto z = std::vector<int>();` then `z.push_back(…)` and `z.append(…)`. The
/// first must name `push_back` on the vector; the second must say the member is absent rather than answer with the nine
/// `basic_string::append` overloads that the name query finds in a standard library the project has read.
///
/// This is a handshake test rather than a unit one because `hover` is written against `Session<DiskFiles>` — the
/// provider a server uses — so the honest way to exercise it is through a server.
#[test]
fn a_member_hover_is_resolved_through_the_objects_type() {
    let project = Project::new("member-hover");
    let source = "\
namespace std {
template <class T>
class vector {
public:
    void push_back(T value);
};
}
void f() {
    auto z = std::vector<int>();
    z.push_back(1);
    z.append(2);
}
";
    project.write("a.cpp", source);
    let mut server = Server::start(project.root());
    let a_uri = uri_of(&project.root().join("a.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {},
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": a_uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );

    let line_of = |needle: &str| -> u32 {
        source
            .lines()
            .position(|line| line.contains(needle))
            .expect("the fixture has the line") as u32
    };
    // **Asked until it answers.** A hover before the project has been read is `null` — the analysis has no view yet —
    // and a well-behaved client asks again as it types; asserting the first answer would be asserting the machine's
    // speed rather than the server's behaviour.
    let mut hover_on = |id: i64, line: u32, character: u32| -> serde_json::Value {
        let uri = a_uri.clone();
        server.ask_until_it(
            id,
            move |_asked| {
                json!({
                    "method": "textDocument/hover",
                    "params": {
                        "textDocument": { "uri": uri },
                        "position": { "line": line, "character": character },
                    },
                })
            },
            |response| !response["result"].is_null(),
        )
    };

    // `z.push_back` — the member, on the type the `auto` initialiser gave it.
    let member = hover_on(2, line_of("z.push_back"), 6);
    let text = member["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover is expected: {member}"))
        .to_string();
    assert!(
        text.contains("push_back"),
        "the member is the answer: {text}"
    );

    // `z.append` — absent, said plainly, and **not** another class's member.
    let absent = hover_on(3, line_of("z.append"), 6);
    let text = absent["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover is expected: {absent}"))
        .to_string();
    assert!(
        text.contains("no member `append`") || text.contains("not known here"),
        "an absent member is answered honestly: {text}"
    );
    assert!(
        !text.contains("basic_string"),
        "and never with another class's members: {text}"
    );
}

/// **A completion inside a format string offers the grammar, and only the grammar.**
///
/// `printf("%5.` is followed by a conversion letter and by nothing else, so the language's own list — the names in
/// scope — is a list of things that cannot be written there. The request is asked at three positions that are one
/// character apart and the answers differ because the grammar does:
///
/// ```text
///   printf("%|")     every conversion, with what each one means
///   printf("%5.|")   the same, after the precision
///   printf("%d|")    nothing, and *nothing* is the answer — not the names in scope
/// ```
///
/// The third is the one that matters: a fall-through to the ordinary list there is the defect this branch exists to
/// prevent, and a client that received it would show `printf`, `n`, `main` inside a format string.
#[test]
fn a_completion_inside_a_format_string_is_the_grammar_s_own() {
    const FIXTURE: &str = "\
#include <cstdio>
struct width { int value; };
void f(double d, int n) {
    std::printf(\"%5.\", d);
}
";
    let project = Project::new("completion-format");
    project.write("main.cpp", FIXTURE);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
                "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": FIXTURE }
        }),
    );

    // **The cursor goes just before the closing quote**, which is where a conversion belongs: `"%5.` has its width and
    // its precision and is waiting for the letter.
    let line = line_of(FIXTURE, "%5.");
    let character = FIXTURE
        .lines()
        .nth(line as usize)
        .and_then(|text| text.find("%5."))
        .expect("the fixture writes it")
        + 3;
    let ask = |server: &mut Server, id: i64| -> Vec<Value> {
        let answer = server.ask_until(id, |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                },
            })
        });
        answer["result"]["items"]
            .as_array()
            .or_else(|| answer["result"].as_array())
            .cloned()
            .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"))
    };

    let items = ask(&mut server, 100);
    assert!(!items.is_empty(), "a conversion list was expected");
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();
    for conversion in ["d", "f", "s", "x", "p"] {
        assert!(
            labels.contains(&conversion),
            "`{conversion}` is a conversion: {labels:?}"
        );
    }
    assert!(
        !labels.contains(&"printf"),
        "and the names in scope are not offered here: {labels:?}"
    );
    // The item says what the conversion does, which is the reason the list is worth showing at all.
    let fixed = items
        .iter()
        .find(|item| item["label"] == json!("f"))
        .expect("`f` is in the list");
    assert!(
        fixed["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("fixed-point")),
        "the one-line meaning: {fixed}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The three things a C reader asks of a line they are writing** — completion of a name the file itself defines,
/// completion inside a macro call, and the signature of a standard-library function.
///
/// ```c
/// #include <stdio.h>
/// #define LOCAL_MACRO(n) ((n) + 1)
/// static int local_helper(int n) { return n; }
/// int main(void) {
///     int value = local_helper(1);
///     fprintf(stdout, "%d\n", value);      signature help at the `(`
///     return LOCAL_MACRO(value);           completion after `LOCAL_`
/// }
/// ```
///
/// Reported together: *"当前文件定义的内容几乎都无法补全, 但是 hover 和跳转都照得到"* and *"fprintf 没有
/// signatureHelper"*. Hover and go-to-definition working while completion answers nothing is a **handler** question
/// rather than an analysis one — the three queries share the file's view and the offset — so both are asked here, over
/// stdio, at the positions the report names.
#[test]
fn a_c_reader_gets_completion_and_signature_help_for_what_the_file_defines() {
    let project = Project::new("c-completion");
    project.write("main.c", C_COMPLETION_C);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.c"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": C_COMPLETION_C }
        }),
    );

    let at = |needle: &str, after: usize| -> (u64, u64) {
        let line = line_of(C_COMPLETION_C, needle);
        let character = C_COMPLETION_C
            .lines()
            .nth(line as usize)
            .and_then(|text| text.find(needle))
            .expect("the fixture writes it")
            + after;
        (line, character as u64)
    };

    // **A name this file defines**, typed as far as `local_he`.
    let (line, character) = at("local_helper(1)", 7);
    let asked = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            },
        })
    });
    let items = completion_items(&asked);
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();
    assert!(
        labels.contains(&"local_helper"),
        "a function the file defines completes: {} item(s), first {:?}",
        labels.len(),
        labels.iter().take(10).collect::<Vec<_>>()
    );

    // **The same, for a macro** — the row the report names as missing.
    let (line, character) = at("LOCAL_MACRO(value)", 6);
    let asked = server.ask_until(200, |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            },
        })
    });
    let items = completion_items(&asked);
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();
    assert!(
        labels.contains(&"LOCAL_MACRO"),
        "a macro the file defines completes: {} item(s), first {:?}",
        labels.len(),
        labels.iter().take(10).collect::<Vec<_>>()
    );

    // **The empty-prefix list must still reach the names of a header that is not the first one.** Five standard
    // headers declare far more than the 200-item budget, so a first-come cut keeps only `<stdio.h>`'s `a`-`b` names and
    // `fprintf` is absent — which is what a reader sees as "completion does not work in this function". Measured through
    // the real server on the corpus: `fprintf: false` with 200 items before the budget was spent fairly.
    let (line, character) = at("fprintf(stdout", 8);
    let asked = server.ask_until(250, |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            },
        })
    });
    let items = completion_items(&asked);
    assert!(
        items
            .iter()
            .any(|item| item["label"].as_str() == Some("fprintf")),
        "with nothing typed, the budget still reaches `fprintf`: {} item(s)",
        items.len()
    );

    // **`fprintf(`'s signature**, which comes from a header the file includes rather than from the file.
    let (line, character) = at("fprintf(stdout", 8);
    let asked = server.ask_until(300, |id| {
        json!({
            "id": id,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            },
        })
    });
    let signatures = asked["result"]["signatures"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !signatures.is_empty(),
        "`fprintf` has a signature to show: {asked}"
    );
    let label = signatures[0]["label"].as_str().unwrap_or_default();
    // **The parameter's spelling is the implementation's.** The claim is that the label is `fprintf`'s own
    // declaration — the one with a format parameter and a variadic tail — and not the file's `local_helper` or a
    // neighbour in `<stdio.h>`. How that parameter is *spelled* is not this server's: glibc writes `format`, MSVC's
    // UCRT writes `_Format`, and musl writes `__format`. Matching the lowercased label asks the question the test
    // means and answers the same on all three.
    let folded = label.to_lowercase();
    assert!(
        folded.contains("fprintf") && folded.contains("format"),
        "and it is `fprintf`'s own: {label}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The first completion after a file is opened**, asked the way an editor asks it: one shot, no retry, at the
/// keystroke that follows `didOpen`.
///
/// Reported: *"我输入 f 还是没有补全,但是输入到 fp 的时候就有补全了"* — typing `f` shows nothing and typing `fp` works.
/// The two requests differ only in the prefix, so the difference has to be either the answer for `f` or the state the
/// server was in when it arrived; a test that retried either would hide both. So this asks **once** per prefix, in the
/// order a reader types them, and requires the same name from both.
#[test]
fn the_first_completion_after_an_open_answers_the_same_as_the_second() {
    let project = Project::new("c-first-completion");
    project.write("main.c", C_COMPLETION_C);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.c"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": C_COMPLETION_C }
        }),
    );

    // **`f`, then `fp`** — the two prefixes in the report, one shot each, at the same line.
    let line = line_of(C_COMPLETION_C, "fprintf(stdout");
    let labels_at = |server: &mut Server, id: i64, width: u32| -> Vec<String> {
        let character = C_COMPLETION_C
            .lines()
            .nth(line as usize)
            .and_then(|text| text.find("fprintf"))
            .expect("the fixture writes it") as u32
            + width;
        let answer = server.request(
            id,
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        );
        let items = if let Some(items) = answer["result"]["items"].as_array() {
            items.clone()
        } else {
            answer["result"].as_array().cloned().unwrap_or_default()
        };
        items
            .iter()
            .filter_map(|item| item["label"].as_str().map(str::to_string))
            .collect()
    };

    let one = labels_at(&mut server, 100, 1);
    let two = labels_at(&mut server, 200, 2);

    assert!(
        one.iter().any(|label| label == "fprintf"),
        "the answer to `f` holds `fprintf` — {} item(s), first {:?}",
        one.len(),
        one.iter().take(8).collect::<Vec<_>>()
    );
    assert!(
        two.iter().any(|label| label == "fprintf"),
        "and so does the answer to `fp` — {} item(s)",
        two.len()
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A documentation comment's completion carries the edit that replaces the word being typed.**
///
/// Reported from an editor: after `@see`, accepting a name with Tab "把前面的补全吃掉" — it removed the text before the
/// cursor. The cause was a range: the doc reader measured the word from the comment's text, but the name query ran at
/// **offset 0** (a comment has no tree node to ask about), and its answer carried a range from the top of the file,
/// which the client then replaced. The item has to say *where* it goes — a `textEdit` over the typed word — and not
/// leave the client to insert at the cursor.
#[ignore]
#[test]
fn a_doc_links_completion_carries_the_edit_for_the_word() {
    let project = Project::new("doc-link-edit");
    let source = "/** A ship. @see Widget */\nstruct Widget { int size; };\n";
    project.write("a.cpp", source);
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("a.cpp"));

    let capabilities = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "textDocument": { "completion": { "completionItem": { "textEditSupport": true } } },
            },
        }),
    );
    assert!(capabilities["result"].is_object(), "{capabilities}");
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": source } }),
    );

    // The cursor is at the end of `Wid`, the word the reader has typed after `@see`.
    let character = "/** A ship. @see Wid".len() as u64;
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": 0, "character": character },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("Widget")))
        },
    );

    let items = answer["result"]["items"].as_array().expect("a list");
    let item = items
        .iter()
        .find(|item| item["label"] == json!("Widget"))
        .expect("the closure accepted this");
    let edit = &item["textEdit"];
    assert!(
        edit.is_object(),
        "the client must be told where the name goes: {item}"
    );
    assert_eq!(
        edit["range"]["start"],
        json!({ "line": 0, "character": character - 3 }),
        "the edit starts at the word, not at the `@see` before it: {item}"
    );
    assert_eq!(
        edit["range"]["end"],
        json!({ "line": 0, "character": character }),
        "{item}"
    );
    assert_eq!(edit["newText"], json!("Widget"), "{item}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A member of a class template is answered for the instantiation the reader wrote.**
///
/// Reported from an editor on `test_cpp/src/templates.cpp`: hovering `Builder<std::string>::build()` answered
/// `template <typename T> T build(…)` — the *definition's* fact, with the class's parameter still in the return
/// type — and said nothing about `std::string`. The argument is written in the **qualifier**, which is why the
/// class-template rule in the card (it reads a `Type` fact's own spelling) never saw it.
///
/// The answer is the member list's own substitution: `members_of("Builder<std::string>")` holds `build` with
/// `returns: std::string`, and the card uses that fact.
#[test]
fn a_hover_on_a_member_of_an_instantiation_names_the_instantiation() {
    let project = Project::new("hover-instantiation-member");
    project.write(
        "main.cpp",
        "\
template <typename T>
struct Builder {
    T value{};

    static T build();
};

template <typename T>
T Builder<T>::build() {
    return T{};
}

int main() {
    return static_cast<int>(Builder<int>::build());
}
",
    );

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let source = "\
template <typename T>
struct Builder {
    T value{};

    static T build();
};

template <typename T>
T Builder<T>::build() {
    return T{};
}

int main() {
    return static_cast<int>(Builder<int>::build());
}
";
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "hover": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );

    // The cursor is on `build` in the **use**, whose line is the last one.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/hover",
                "params": {
                    "textDocument": { "uri": main_uri },
                    // 0-based, and the use is the file's last line (the 15th).
                    "position": { "line": 13, "character": 42 },
                },
            })
        },
        |answer| !answer["result"].is_null(),
    );
    let markdown = answer["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover was expected, got {answer}"));
    assert!(
        markdown.contains("int build()"),
        "the instantiation's own signature, not the parameter name: {markdown}"
    );
    assert!(
        !markdown.contains("T build()"),
        "and nothing of the declaration's parameter is left in it: {markdown}"
    );
}
