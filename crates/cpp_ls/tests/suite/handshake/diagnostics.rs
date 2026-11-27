//! Pull and push diagnostics, and findings about text that no longer exists.

use super::fixtures::*;
use super::support::*;

/// **A client that pulls gets a report, not `null`.**
///
/// The reported symptom was that diagnostics did not change while typing, and this is the layer it was in. A client
/// that advertises `textDocument.diagnostic` is answered by `textDocument/diagnostic`, and that handler went through
/// `snapshot_query` → `query_blocking` → `RequestOutcome::Missing`, which `ServerContext::task` turns into
/// **`Response::new_ok(id, Value::Null)`**.
///
/// `null` is not a `DocumentDiagnosticReport`. A client that asked for one and got `null` cannot tell it from a
/// failure, so it keeps the diagnostics it already had — which is precisely "输入代码之后诊断不改变".
///
/// There were **two** ways to reach `Missing` and both are fixed in the handler:
///
/// ```text
/// the file was never read         `prepare` queues cooking and returns, so the query found no reading
/// `diagnose_file` answered None   …which was then reported as `Missing` rather than as "nothing wrong"
/// ```
///
/// So this asserts what a pull client needs: the answer is an **object with an `items` array** (the shape the
/// protocol requires), it is non-empty for a file with a parse error, and **asking twice gives the same answer** —
/// a pulling client re-asks after every edit, and a screen that flickers between answers is one nobody trusts.
///
/// This is also the test that says the push half is gone: it declares the pull capability, and no test in this file
/// waits for a `publishDiagnostics` notification any more, because the server does not send one.
#[test]
fn a_pulling_client_gets_a_report_and_not_null() {
    let project = Project::new("pull-diagnostics");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                // **The pull capability**, which is what routes diagnostics to the request instead of the
                // notification. Its absence is what the other tests exercise.
                "textDocument": { "diagnostic": { "dynamicRegistration": false } },
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert_eq!(
        initialized["result"]["capabilities"]["diagnosticProvider"]["interFileDependencies"],
        json!(true),
        "the server advertises the provider a pulling client looks for: {initialized}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    let report = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/diagnostic",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });

    assert!(
        !report["result"].is_null(),
        "`null` is what a client cannot act on, and it is what this answered: {report}"
    );
    let items = report["result"]["items"].as_array().unwrap_or_else(|| {
        panic!("a full document diagnostic report has an `items` array: {report}")
    });
    assert!(
        !items.is_empty(),
        "the file has an unclosed class, so a report with no items is \"clean\" and wrong: {report}"
    );
    assert!(
        items.iter().any(|item| item["message"]
            .as_str()
            .is_some_and(|message| message.contains(';'))),
        "and the parse error is the one the push test also expects: {report}"
    );

    // --- **Asking twice is answered twice, and the second answer is the same** -----------------------
    //
    // A pulling client re-asks after every edit, so the request is made repeatedly against unchanged text. The
    // answer must be the *same* answer and not, say, one that depends on whether the reading happened to be cached —
    // which is the property that makes a pull client's screen stable.
    //
    // **And it is where a duplicate was found**: measured, this same file reported its `expected \`;\`` **twice** on
    // the second ask. `prepare_for_a_diagnostic` re-reads and re-cooks the file, and a path that is cooked twice can
    // be counted twice by whatever reads the errors.
    // asserted about it.
    let again = server.ask_until(200, |id| {
        json!({
            "id": id,
            "method": "textDocument/diagnostic",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });
    assert_eq!(
        again["result"]["items"].as_array().map(Vec::len),
        Some(items.len()),
        "the same text has to give the same answer twice: first {report}, then {again}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

#[test]
fn a_client_can_open_a_file_and_get_diagnostics_a_definition_and_a_hover() {
    let project = Project::new("handshake");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    // --- initialize -------------------------------------------------------------------------------
    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                // No `textDocument.diagnostic`: this client wants diagnostics **pushed**, which is the half of the
                // diagnostic design that has no request in it.
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );

    let capabilities = &initialized["result"]["capabilities"];
    assert_eq!(capabilities["definitionProvider"], json!(true));
    assert_eq!(capabilities["hoverProvider"], json!(true));
    assert_eq!(
        capabilities["diagnosticProvider"]["workspaceDiagnostics"],
        json!(false),
        "a file's parse errors are the whole answer, so there is nothing a workspace request would add"
    );
    assert!(
        capabilities["textDocumentSync"].is_object(),
        "the document lifecycle is what every other capability needs: {capabilities}"
    );

    server.notify("initialized", json!({}));

    // --- one open document ------------------------------------------------------------------------
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    let reported = server.pull_diagnostics(90, &main_uri);
    assert!(
        !reported.is_empty(),
        "the last class is never closed, so a parse error is expected: {reported:?}"
    );
    assert_eq!(
        reported[0]["severity"],
        json!(1),
        "a parse error is an error, not a warning"
    );
    assert_eq!(
        reported[0]["range"]["start"]["line"],
        json!(6),
        "the error is on the last line: {reported:?}"
    );
    assert!(
        reported[0]["message"]
            .as_str()
            .is_some_and(|message| message.contains(';')),
        "and it is the parser's own message about the missing `;`: {reported:?}"
    );

    // --- definition: the member declared in the included header ------------------------------------
    let location = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 4, "character": 6 },  // `w.size`
            },
        })
    });

    let found_uri = location["result"]["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("a definition was expected, got {location}"));
    assert!(
        found_uri.ends_with("widget.h"),
        "`size` is declared in the header: {location}"
    );
    assert_eq!(
        location["result"]["range"]["start"]["line"],
        json!(0),
        "and the range is the name in that file, not the top of it: {location}"
    );
    assert_eq!(location["result"]["range"]["start"]["character"], json!(20));

    // --- definition on the `#include` itself: the header it names -----------------------------------
    // The fourth answer a jump can give, and the one that used to be missing entirely: `#include "widget.h"`
    // **declares nothing**, so the scope walk, the index by name and the macro table all answer "nothing found"
    // for a line whose whole purpose is to name a file. The cursor is inside the spelling (character 12 is the `d`
    // of `widget.h`), and the range that comes back is the file's first line rather than a point at its start —
    // an editor *selects* a scalar location, and a zero-width range would select nothing.
    let header = server.ask_until(150, |id| {
        json!({
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 0, "character": 12 },  // inside `"widget.h"`
            },
        })
    });

    let header_uri = header["result"]["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("a header was expected, got {header}"));
    assert!(
        header_uri.ends_with("widget.h"),
        "the include names widget.h: {header}"
    );
    assert_eq!(
        header["result"]["range"]["start"],
        json!({ "line": 0, "character": 0 }),
        "from the top of the header: {header}"
    );
    assert_eq!(
        header["result"]["range"]["end"],
        json!({ "line": 1, "character": 0 }),
        "to the start of the second line, which is where the header's first line ends: {header}"
    );

    // --- hover on the same name: **which file the search found** ------------------------------------
    // The other half of a jump whose target the reader cannot see: the line they wrote names a *spelling*, and a
    // search turns it into one of the thousands of files on this machine's include path. The popup says which.
    let about_the_header = server.ask_until(170, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 0, "character": 12 },
            },
        })
    });
    let markdown = about_the_header["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover was expected, got {about_the_header}"));
    assert!(
        markdown.contains("#include \"widget.h\""),
        "the line as the file writes it: {markdown}"
    );
    // The path is compared **case-insensitively**, because the session resolves it through the VFS and the
    // temporary directory the fixture lives in is spelled in lower case there on Windows. What is being asserted
    // is that the popup carries the resolved *file*, not the spelling the reader wrote.
    let resolved = markdown.to_lowercase();
    assert!(
        resolved.contains("widget.h") && resolved.contains("cppls-handshake-tests"),
        "and where it resolved to: {markdown}"
    );

    // The `#` is part of the directive rather than of the name, and the analysis answers `null` there — which is
    // the same "nothing to go to" it gave before this round for the *whole* line. Asserted so that the boundary is
    // pinned: a client that sends a ctrl-click on the `#` must not be sent to a file the analysis guessed at.
    let on_the_hash = server.request(
        160,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 0, "character": 0 },
        }),
    );
    assert_eq!(
        on_the_hash["result"],
        Value::Null,
        "the `#` is not the header's name: {on_the_hash}"
    );

    // --- hover: the type of a local, then the declaration of the member ---------------------------
    // The two positions below are `Widget w;` on line 3 and the `size` of `w.size` on line 4.
    let hover = server.ask_until(200, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 3, "character": 6 },  // `Widget w;`
            },
        })
    });
    let markdown = hover["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover was expected, got {hover}"));
    assert!(
        markdown.contains("struct Widget"),
        "the declaration as the file writes it: {markdown}"
    );
    assert!(markdown.contains("widget.h"), "{markdown}");

    let member = server.ask_until(300, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 4, "character": 7 },  // `w.size`
            },
        })
    });
    let markdown = member["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover for the member was expected, got {member}"));
    assert!(markdown.contains("size"), "{markdown}");
    assert!(markdown.contains("int"), "its type is shown: {markdown}");

    // --- a hover where there is nothing to say answers null, rather than an empty popup ------------
    let nothing = server.request(
        900,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 1, "character": 0 },  // the blank line
        }),
    );
    assert_eq!(
        nothing["result"],
        Value::Null,
        "there is nothing at the position, and that is what the client is told"
    );

    // --- shutdown ---------------------------------------------------------------------------------
    let shutdown = server.request(901, "shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    server.notify("exit", Value::Null);
    assert!(
        server.wait_for_exit(),
        "the server exits when the client says exit"
    );
}

/// **A branch nobody takes is answered as clean.**
///
/// The two readings disagree about this file, and that disagreement is the reason the diagnostics channel reads the
/// cooked one: the raw reading reports the unclosed class — correctly, the *text* says so — and a compiler never
/// sees that branch. A server that answered from the raw reading would put a red squiggle on code that is not
/// compiled, which is the false positive the whole second reading exists to remove.
///
/// **Pull makes this the first answer rather than a later one.** The handler reads and cooks the file before it
/// queries, so there is no window in which the raw answer could reach a client — which is what the push path had to
/// live with, and what made this test above say "the assertion is about the *last* thing the client is told".
#[test]
fn a_branch_nobody_takes_is_answered_as_clean() {
    let project = Project::new("cooked-diagnostics");
    project.write("cfg.h", CFG_H);
    project.write("main.cpp", BRANCH_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                // No `textDocument.diagnostic`: this client wants diagnostics **pushed**, which is the half that has
                // no request in it — and the half a re-publish after the drain has to work for.
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": BRANCH_CPP }
        }),
    );

    let clean = server.pull_diagnostics(90, &main_uri);
    assert!(
        clean.is_empty(),
        "the branch is not compiled, so there is nothing to report: {clean:?}"
    );

    // **The falsification lives one layer down**, where the same fixture is read both ways:
    // `cpp_code_analysis::session::tests::a_branch_nobody_takes_reports_nothing_once_the_file_is_cooked` asserts
    // that this file's *raw* reading reports exactly that error and its cooked reading reports nothing. Without it
    // this test would also pass on a file that simply has no error in it.

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **An error typed into an open file is published, and the edit has not been cooked yet.**
///
/// Reported: `std::cout << middleName << std::endl` with no `;` sat there with no diagnostic, however long the user
/// waited — and this is the case the older diagnostics test does not reach, because it opens a file whose text is
/// already on disk.
///
/// What makes it different is that **an edit drops the cooked reading**, and a diagnostic is answered from that
/// reading. So the pass that runs after the typing stops found no reading at all, `diagnose_file` answered `None`,
/// and the service logged *"nothing to diagnose"* and published nothing. The reading comes back when the index pump
/// reaches the file, and the pump works through its queue rather than the editor.
///
/// Measured on the fixture, at the analysis layer:
///
/// ```text
/// before the edit:       Some(1)
/// right after the edit:  None       ← what was being published as nothing
/// after re-indexing:     Some(1)
/// ```
///
/// So this opens a file that is **clean**, edits it into having a missing `;`, and requires the client to be told
/// about it. A server that waits for a re-read fails here and passes the fixture whose text was already on disk.
#[test]
fn an_error_typed_into_an_open_file_is_answered() {
    let project = Project::new("diagnostics-after-an-edit");
    project.write("main.cpp", CLEAN_CPP);

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": CLEAN_CPP }
        }),
    );

    // **The edit**, as a client sends it: the whole text, one version later. `EDITED_CPP` differs from `CLEAN_CPP` by
    // a missing `;` and nothing else, so an error reported here is about this edit.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 2 },
            "contentChanges": [{ "text": EDITED_CPP }],
        }),
    );

    let diagnostics = server.pull_diagnostics(300, &main_uri);
    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic["message"]
            .as_str()
            .is_some_and(|message| message.contains(';'))),
        "the missing `;` is the only difference from the clean file: {diagnostics:?}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A diagnostic about text that is no longer there must go away on the next pull.**
///
/// Reported from an editor, with a screenshot: the file had been typed into and corrected, and it was completely
/// correct — and the Problems panel still showed the two findings from the moment in between (`missing-return` on
/// `int main() {` and `expected ';' after expression` on the `return 0;` line). Editing again did not always clear
/// them. That is the worst failure this channel has, because a reader has no way to tell a stale squiggle from a
/// real one, and every later finding becomes untrustworthy.
///
/// The cause was not a `resultId` (this server sends `resultId: None` and always a **full** report). It was the
/// handler's own rule: `document_diagnostic::the_report` **repeated** the last report when the analysis had no answer
/// and **unioned** it with the current one when the checks were withheld. Both were written to stop diagnostics
/// flashing while the index filled, and both keep a finding about deleted text alive indefinitely.
///
/// The sequence is the reported one, in order: the file is clean, an edit breaks it, the edit is taken back.
#[test]
fn a_diagnostic_about_deleted_text_is_gone_by_the_next_pull() {
    let project = Project::new("diagnostics-clear-after-a-fix");
    project.write("main.cpp", CLEAN_CPP);

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": CLEAN_CPP }
        }),
    );

    // 1. The clean file, pulled once so the server has a report to be stale about.
    server.pull_diagnostics(100, &main_uri);

    // 2. The edit that breaks it — one `;` short.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 2 },
            "contentChanges": [{ "text": EDITED_CPP }],
        }),
    );
    let broken = server.pull_diagnostics(200, &main_uri);
    assert!(
        broken
            .iter()
            .any(|diagnostic| diagnostic["message"].is_string()),
        "the missing `;` is found: {broken:?}"
    );

    // 3. The fix — the file the session started with, byte for byte.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 3 },
            "contentChanges": [{ "text": CLEAN_CPP }],
        }),
    );

    // **What a reader must see: no diagnostic about text that is gone.** A file this small has nothing else to
    // report — no includes, no checks that can fire — so the whole list has to be empty.
    let fixed = server.pull_diagnostics(300, &main_uri);
    assert!(
        fixed.is_empty(),
        "the file is byte-for-byte the one that was clean before the edit, so nothing may be reported: {fixed:?}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A pull while the index is still busy must not answer with a finding about text that is gone.**
///
/// The same reported sequence as above, on a project whose index **does not settle** when the pull arrives: the
/// handler decides `settled` from `Session::pending_work() == 0`, and a project of eighty headers is still being
/// read. That state is not exotic — it is what a real project looks like for its first seconds and, on a big one,
/// for minutes — and in it the handler used to answer with the **union** of the current findings and the last
/// report it had sent. A finding about a `;` the reader deleted an edit ago was in the union, and nothing removed
/// it: the client had no reason to ask again, because it had just asked.
///
/// So this is the same bug as the test above, at the state that makes it *permanent* rather than momentary. The
/// two tests are kept apart because they can fail independently: one is the report's rule, the other is when the
/// rule is reached.
#[test]
fn a_finding_about_deleted_text_does_not_survive_a_busy_index() {
    let project = Project::new("diagnostics-while-busy");

    // **Eighty headers in a chain**, so the session has work left when the pull arrives — `PREPARE_INDEX_SLICE` is
    // 24 files, and the pull's own slice drains a quarter of it. Each header includes the next, so main.cpp's
    // closure is the whole chain.
    for index in 0..80 {
        let next = if index + 1 < 80 {
            format!("#include \"h{}.h\"\n", index + 1)
        } else {
            String::new()
        };
        project.write(
            &format!("h{index}.h"),
            &format!("#pragma once\n{next}struct S{index} {{ int f; }};\n"),
        );
    }
    let clean = "#include \"h0.h\"\nint main() { return 0; }\n";
    let broken = "#include \"h0.h\"\nint main() { return 0 }\n";
    project.write("main.cpp", clean);

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": clean }
        }),
    );
    server.pull_diagnostics(100, &main_uri);

    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 2 },
            "contentChanges": [{ "text": broken }],
        }),
    );
    let found = server.pull_diagnostics(200, &main_uri);
    assert!(
        found
            .iter()
            .any(|diagnostic| diagnostic["message"].is_string()),
        "the missing `;` is a parse error: {found:?}"
    );

    // The fix, as the editor sends it — the file the session opened, byte for byte.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 3 },
            "contentChanges": [{ "text": clean }],
        }),
    );
    let fixed = server.pull_diagnostics(300, &main_uri);
    assert!(
        fixed.is_empty(),
        "the `;` is back and the file is the one that was clean before the edit, so nothing may be reported: \
         {fixed:?}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The reported file is answered — and the answer is an empty list, which is correct.**
///
/// Reported twice: first a `return 0` with no `;` produced no diagnostic, and after that was fixed the file *with*
/// its semicolons still produced none. The two have different causes, and the second is not a defect:
///
/// ```text
/// the file parses                 so `syntax-error` is absent — a fact, because the parser defect is fixed
/// the only check that fires      `IncludeNotFound`, which is about the machine's include paths
/// on a machine that has them     nothing at all
/// ```
///
/// Logged from a run of this fixture, with the probes that established it:
///
/// ```text
/// COOK PROBE: summary=true cooked=true pending=0
/// READ PROBE: held=true key=Some("…/main.cpp") checks=Some((0, 0, Cooked))
/// ```
///
/// `checks=0` with the reading **cooked** and the file **held** is the whole answer: the channel is working and the
/// file is clean. So this test asserts the thing that is actually true and was worth doubting — that the server
/// **publishes**, that the publish is stamped with the document's version, and that the answer for this text is the
/// empty list rather than no message at all. A server that never reached the client, and one that answered "clean",
/// are the two states the report could not tell apart, and only the first is a bug.
///
/// What the file *does* deserve — `full.size();` discarding its result three times — is a rule this layer does not
/// have. `cpp_code_analysis`'s `the_reported_file_reports_only_its_unresolved_includes` records that.
#[test]
fn the_reported_file_is_answered() {
    let project = Project::new("reported-file");
    project.write("main.cpp", REPORTED_CPP);

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": REPORTED_CPP }
        }),
    );

    let diagnostics = server.pull_diagnostics(90, &main_uri);
    eprintln!(
        "the server answered with {} diagnostic(s): {diagnostics:?}",
        diagnostics.len()
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic["code"] != json!("syntax-error")),
        "the file parses, and `return` reporting a missing `;` is the defect this fixture was built from: \
         {diagnostics:?}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

#[test]
fn a_client_without_pull_support_receives_versioned_diagnostics() {
    let project = Project::new("legacy-diagnostics");
    let broken = "struct Unclosed { int value;\n";
    let clean = "int main() { return 0; }\n";
    project.write("main.cpp", broken);
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "textDocument": { "publishDiagnostics": { "versionSupport": true } }
            }
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": broken }
        }),
    );
    let first = server.wait_for(|message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == uri
            && message["params"]["version"] == 1
    });
    assert!(
        first["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "the invalid file must be diagnosed: {first}"
    );

    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": clean }]
        }),
    );
    let corrected = server.wait_for(|message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == uri
            && message["params"]["version"] == 2
    });
    assert_eq!(corrected["params"]["diagnostics"], json!([]));

    for (version, text) in [(3, broken), (4, clean)] {
        server.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }),
        );
    }
    let latest = server.wait_for(|message| {
        if message["method"] == "textDocument/publishDiagnostics" && message["params"]["uri"] == uri
        {
            assert_ne!(
                message["params"]["version"], 3,
                "a superseded result was published"
            );
            return message["params"]["version"] == 4;
        }
        false
    });
    assert_eq!(latest["params"]["diagnostics"], json!([]));

    server.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    );
    let closed = server.wait_for(|message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == uri
            && message["params"]["version"].is_null()
    });
    assert_eq!(closed["params"]["diagnostics"], json!([]));
}

#[test]
fn an_unreadable_diagnostic_target_is_an_error_not_an_empty_report() {
    let project = Project::new("missing-diagnostic");
    let mut server = Server::start(project.root());
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
    let missing = uri_of(&project.root().join("missing.cpp"));
    let response = server.request(
        2,
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": missing } }),
    );
    assert_eq!(
        response["error"]["code"],
        json!(lsp_server::ErrorCode::RequestFailed as i32),
        "an unreadable file is not a clean file: {response}"
    );
}

/// **A declaration's own line, when the file it is in is the file being read.**
///
/// The tooltip's last line is `file:line:column`, and the line is the one part of this server that can be wrong
/// without anything looking broken: a fact about a declaration in **another** file carries that file's offsets,
/// while a fact about a declaration in **the file being viewed** comes from the view's own tree — whose offsets are
/// the **rendering's**, because the rendering is what the parser was handed, and whose line numbers are therefore
/// short by however many `#include` and `#define` lines the directives took out.
///
/// Measured on a real report: a class on line 6 of a file with five `#include`s above it was announced as
/// `main.cpp:1:7` — **the column right and the line five short**, which is exactly the number of lines between them.
/// The existing hover test cannot catch it: its declaration is in a header, so it takes the other branch.
///
/// The criterion is the line and the column together, because the column is what was right in the report — a test
/// that only checked "which file" would pass on the broken answer.
#[test]
fn a_declaration_in_the_viewed_file_is_reported_on_its_own_line() {
    let project = Project::new("hover-coordinates");
    // Five lines of directives above the class, so a rendering that drops them is five lines short — the number in
    // the report, and the number that makes the difference visible rather than rounded away.
    project.write(
        "main.cpp",
        "#include <vector>\n\
         #include <string>\n\
         #include <format>\n\
         #include <iostream>\n\
         \n\
         class Sux {\n\
         public:\n\
             void print() {}\n\
         };\n\
         \n\
         int main() {\n\
             Sux sux;\n\
             return 0;\n\
         }\n",
    );

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

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
            "textDocument": {
                "uri": main_uri,
                "languageId": "cpp",
                "version": 1,
                "text": "#include <vector>\n#include <string>\n#include <format>\n#include <iostream>\n\n\
                         class Sux {\npublic:\n    void print() {}\n};\n\nint main() {\n    Sux sux;\n    \
                         return 0;\n}\n",
            }
        }),
    );

    // **Asked until the file has been read the way a client that waited gets it.**
    //
    // `Session::view` answers from the file's own tokens until a rendering exists and builds it for the next query —
    // a deliberate cost, because a rendering cannot be built behind an `&self`. In that first reading every offset is
    // the file's, so both readers below agree by accident and a test that asked once would pass on the broken answer.
    // Measured: with the fix disabled this test still passed, which is what sent it back here.
    // **The deadline of a test that reads a standard-library header**, which this one does four times over
    // (`<vector>`, `<string>`, `<format>`, `<iostream>`): measured alone, 27 s, and under `cargo test`'s parallel load
    // the same wait went over the 30 s `TIMEOUT` and failed the suite. See [`STD_LIBRARY_TIMEOUT`], which exists for
    // exactly this and is already used by the tests whose subject is that slow.
    let mut answer = server.ask_until_it_within(
        100,
        STD_LIBRARY_TIMEOUT,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/hover",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 11, "character": 4 },
                },
            })
        },
        |answer| !answer["result"].is_null(),
    );
    for _ in 0..4 {
        answer = server.ask_until_it_within(
            100,
            STD_LIBRARY_TIMEOUT,
            |id| {
                json!({
                    "id": id,
                    "method": "textDocument/hover",
                    "params": {
                        "textDocument": { "uri": main_uri },
                        "position": { "line": 11, "character": 4 },
                    },
                })
            },
            |answer| !answer["result"].is_null(),
        );
    }

    let markdown = answer["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a hover was expected, got {answer}"));
    println!("--- the popup about `Sux` ---\n{markdown}\n---");
    assert!(
        markdown.contains("main.cpp:6:7"),
        "the class is written on line 6, column 7 — not on a line measured in the rendering: {markdown}"
    );
    // **And the popup's own text, read at the fact's offsets.** Those offsets are the rendering's, so reading the
    // file's bytes at them lands wherever the directives had taken lines out: measured on the same report, a popup
    // about `main` began with `tream>`, the tail of `#include <iostream>`, because the four `#include`s above `main`
    // had moved it 60 bytes. The line *number* was right by then — the two are separate readers and this is the one
    // that was still wrong.
    assert!(
        markdown.contains("class Sux"),
        "the declaration as it is written, not the bytes at that offset in another document: {markdown}"
    );
    assert!(
        !markdown.contains("tream>"),
        "and no fragment of an `#include` line: {markdown}"
    );
}

/// **A request whose params do not parse gets an answer.** The protocol's is `InvalidParams`; before this the request
/// produced **no response at all** — the dispatcher fell out of its arm, logged nothing, and a client waited for ever
/// (measured with a `prepareCallHierarchy` whose `position` was nested one level too deep: the probe hung, and every
/// request after it queued behind the same silence).
///
/// A malformed request is a client bug, but silence turns it into a server that looks hung.
#[test]
fn a_request_with_unreadable_params_is_answered_with_invalid_params() {
    let project = Project::new("invalid_params");
    project.write("main.cpp", "int main() { return 0; }\n");

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

    let uri = uri_of(&project.root().join("main.cpp"));
    // `position` is a *position*, not a `{ textDocument, position }` object — the shape a client gets wrong.
    let answer = server.request(
        2,
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": uri },
            "position": { "position": { "line": 0, "character": 4 } },
        }),
    );
    println!("answer: {answer}");
    assert_eq!(
        answer["error"]["code"].as_i64().map(|code| code as i32),
        Some(lsp_server::ErrorCode::InvalidParams as i32),
        "an unreadable request is InvalidParams, not silence: {answer}"
    );
}

/// **An Ascend C project is read as Ascend C.** `__aicore__`/`__global__`/`__gm__` are qualifiers only in this family,
/// and the configured kernel include is what makes `kernel_operator.h` resolve. A completion after a `LocalTensor` is
/// the end-to-end check that the family reached both the parser and the project configuration.
#[test]
fn an_ascend_project_reads_its_kernel_syntax_and_headers() {
    const HEADER: &str = "\
#pragma once
namespace AscendC {
template <typename T> struct LocalTensor {
    __ubuf__ T *address;
    __ubuf__ T *GetPhyAddr() const { return address; }
};
struct TPipe { };
template <int Position, int Depth> struct TQue { };
}
using GM_ADDR = __gm__ uint8_t *;
";
    const SOURCE: &str = "\
#include \"kernel_operator.h\"
using namespace AscendC;
extern \"C\" __global__ __aicore__ void add_kernel(GM_ADDR x) {
    LocalTensor<float> local;
    local.
    __ai
}
";
    let project = Project::new("ascend-family");
    std::fs::create_dir_all(project.root().join("include")).expect("the include directory");
    project.write(
        ".cppls.toml",
        "[compile.ascend]\nenable = true\ninclude = [\"include\"]\n",
    );
    project.write("include/kernel_operator.h", HEADER);
    let main = project.root().join("add_kernel.cpp");
    project.write("add_kernel.cpp", SOURCE);
    let uri = uri_of(&main);
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
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": SOURCE }
        }),
    );

    let line = SOURCE
        .lines()
        .position(|text| text.trim() == "local.")
        .expect("the access") as u64;
    let character = SOURCE
        .lines()
        .nth(line as usize)
        .and_then(|text| text.find("local."))
        .expect("the access")
        + "local.".len();
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                },
            })
        },
        |response| {
            response["result"]["items"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["label"] == json!("GetPhyAddr"))
            })
        },
    );
    let items = answer["result"]["items"]
        .as_array()
        .expect("the Ascend member list");
    let address = items
        .iter()
        .find(|item| item["label"] == json!("GetPhyAddr"))
        .expect("the LocalTensor member");
    assert_eq!(
        address["detail"], "T * ()",
        "the toolkit type's own signature reaches the list: {address}"
    );

    // **The family's vocabulary is offered too**, not only the toolkit's declarations: `__ai` lists the AICore
    // qualifier with its one-line description.
    let qualifier_line = SOURCE
        .lines()
        .position(|text| text.trim() == "__ai")
        .expect("the qualifier line") as u64;
    let qualifier_character = SOURCE
        .lines()
        .nth(qualifier_line as usize)
        .and_then(|text| text.find("__ai"))
        .expect("the qualifier line")
        + "__ai".len();
    let qualifiers = server.ask_until_it(
        300,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": qualifier_line, "character": qualifier_character },
                },
            })
        },
        |response| {
            response["result"]["items"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["label"] == json!("__aicore__"))
            })
        },
    );
    let qualifier = qualifiers["result"]["items"]
        .as_array()
        .expect("the qualifier list")
        .iter()
        .find(|item| item["label"] == json!("__aicore__"))
        .expect("the AICore qualifier");
    assert!(
        qualifier["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("AICore")),
        "the qualifier's one-line description reaches the list: {qualifier}"
    );
    assert_eq!(qualifier["kind"], 14, "it is a keyword: {qualifier}");
}
