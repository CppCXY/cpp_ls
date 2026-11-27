//! Definition, references, rename, symbols, hierarchies and selection ranges.

use super::fixtures::*;
use super::support::*;

#[test]
fn links_and_type_definitions_are_ready_on_the_first_request_after_open() {
    let project = Project::new("first-links-and-types");
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
            "capabilities": { "textDocument": { "diagnostic": {} } }
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": FRESH_TYPE_CPP }
        }),
    );
    let links = server.request(
        2,
        "textDocument/documentLink",
        json!({ "textDocument": { "uri": main_uri } }),
    );
    assert!(
        links["result"].as_array().is_some_and(|links| {
            links.iter().any(|link| {
                link["target"]
                    .as_str()
                    .is_some_and(|target| target.eq_ignore_ascii_case(&header_uri))
            })
        }),
        "the first request must include the resolved header: {links}"
    );

    let (line, character) = position_of(FRESH_TYPE_CPP, "Widget w");
    let types = server.request(
        3,
        "textDocument/typeDefinition",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": line, "character": character - 1 },
        }),
    );
    assert!(
        types["result"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.eq_ignore_ascii_case(&header_uri))
            })
        }),
        "the first type definition must resolve the included type: {types}"
    );
}

/// **References and rename: the macro is renamed everywhere, the ordinary name is refused.**
///
/// Two features in one test because they are one question asked twice — "where is this name written" — and because
/// the second half is the one that must **not** happen: a rename of an ordinary name would have to decide, for every
/// candidate file, whether the name at that offset resolves to the same declaration, which needs each candidate's
/// scopes and therefore a parse per candidate. So the handler refuses (`null`), and this test pins the refusal next
/// to the success, because "renamed half of the uses" is the one outcome an editor must never produce.
///
/// And a macro's rename *is* exact: its name is a textual thing, one table in the preprocessor, replaced wherever it
/// appears — so every edit below is a place the analysis read as that macro, in the files that can see its
/// definition.
#[test]
fn a_macro_is_renamed_everywhere_and_an_ordinary_name_is_refused() {
    let project = Project::new("rename");
    project.write("limits.h", LIMITS_H);
    project.write("api.h", ITEMS_H);
    project.write("main.cpp", ITEMS_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
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
    let capabilities = &capabilities["result"]["capabilities"];
    assert_eq!(
        capabilities["referencesProvider"],
        json!(true),
        "{capabilities}"
    );
    assert_eq!(
        capabilities["renameProvider"]["prepareProvider"],
        json!(true),
        "the rename box is offered only where a rename can happen: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": ITEMS_CPP }
        }),
    );

    // The use in `main.cpp` is on line 2 at columns 13..22 (`    int more[MAX_ITEMS];`).
    let at_the_use = json!({ "line": 2, "character": 16 });

    // --- references, with and without the definition ------------------------------------------------
    // `ask_until` is what a client does with the answer this server gives while the index still has work: `null`,
    // because a reference list assembled from a partially read index is a false claim rather than a smaller truth
    // (the protocol gives this response no `isIncomplete`, unlike a completion list). The loop below is therefore
    // part of the behaviour under test, not test scaffolding.
    let mut references = |include_declaration: bool| {
        server.ask_until(100, |id| {
            json!({
                "id": id,
                "method": "textDocument/references",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": at_the_use,
                    "context": { "includeDeclaration": include_declaration },
                },
            })
        })
    };

    let with_the_definition = references(true);
    let locations = with_the_definition["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a location list was expected, got {with_the_definition}"));
    assert_eq!(
        locations.len(),
        3,
        "the `#define` and both uses: {with_the_definition}"
    );
    let files: Vec<&str> = locations
        .iter()
        .filter_map(|location| location["uri"].as_str())
        .collect();
    assert!(
        files.iter().any(|file| file.ends_with("limits.h")),
        "including the file that defines it, which nobody opened: {with_the_definition}"
    );
    assert!(
        files.iter().any(|file| file.ends_with("api.h")),
        "and the header that uses it: {with_the_definition}"
    );

    let without = references(false);
    assert_eq!(
        without["result"].as_array().map(Vec::len),
        Some(2),
        "`includeDeclaration: false` leaves the `#define` out: {without}"
    );

    // --- the rename box, and the rename --------------------------------------------------------------
    let prepared = server.request(
        200,
        "textDocument/prepareRename",
        json!({ "textDocument": { "uri": main_uri }, "position": at_the_use }),
    );
    // **`RangeWithPlaceholder`**: the box starts around the name *and* with the name in it, so a reader renames
    // `MAX_ITEMS` rather than typing over an empty box. The server used to answer a bare range — and nothing at all for
    // a symbol.
    assert_eq!(
        prepared["result"]["range"]["start"],
        json!({ "line": 2, "character": 13 }),
        "the box starts around the name itself: {prepared}"
    );
    assert_eq!(
        prepared["result"]["range"]["end"],
        json!({ "line": 2, "character": 22 }),
        "{prepared}"
    );
    assert_eq!(
        prepared["result"]["placeholder"], "MAX_ITEMS",
        "and it starts with the name in it: {prepared}"
    );

    let renamed = server.request(
        300,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": main_uri },
            "position": at_the_use,
            "newName": "MAX_ENTRIES",
        }),
    );
    let changes = renamed["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("a workspace edit was expected, got {renamed}"));
    assert_eq!(changes.len(), 3, "three files are edited: {renamed}");

    let edits: Vec<&Value> = changes
        .values()
        .flat_map(|edits| edits.as_array().into_iter().flatten())
        .collect();
    assert_eq!(edits.len(), 3, "and one edit in each: {renamed}");
    assert!(
        edits
            .iter()
            .all(|edit| edit["newText"] == json!("MAX_ENTRIES")),
        "every edit writes the new name: {renamed}"
    );
    let definition_edit = changes
        .iter()
        .find(|(uri, _)| uri.ends_with("limits.h"))
        .map(|(_, edits)| &edits[0])
        .unwrap_or_else(|| panic!("the `#define` itself is edited: {renamed}"));
    assert_eq!(
        definition_edit["range"],
        json!({
            "start": { "line": 0, "character": 8 },
            "end": { "line": 0, "character": 17 },
        }),
        "and the definition's edit covers its name: {definition_edit}"
    );

    // --- the other half of the feature: a **local** is renamed inside its own file ---------------------
    // `more` is declared and never used in this fixture, which is the case that used to be refused: `local_references_of`
    // records a *use* against its declaration, so an unused local has no entry — and renaming the declaration alone is
    // the whole edit. It must not leave this file: a local is not visible through an include, so a cross-file list
    // would rewrite other files' `more`s.
    let on_a_local = json!({ "line": 2, "character": 9 });

    let prepared = server.request(
        400,
        "textDocument/prepareRename",
        json!({ "textDocument": { "uri": main_uri }, "position": on_a_local }),
    );
    assert_eq!(
        prepared["result"]["placeholder"], "more",
        "a local has a rename box too: {prepared}"
    );

    let renamed = server.request(
        500,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": main_uri },
            "position": on_a_local,
            "newName": "renamed",
        }),
    );
    let changes = renamed["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("the local is renamed: {renamed}"));
    println!("local rename: {renamed}");
    assert_eq!(
        changes.len(),
        1,
        "one file — the one the local is in: {renamed}"
    );
    assert_eq!(
        changes
            .values()
            .next()
            .and_then(|edits| edits.as_array())
            .map(Vec::len),
        Some(1),
        "one edit: the declaration, the only occurrence in this fixture: {renamed}"
    );
    // A rename to something that is not a name is refused too — two identifiers, a literal, nothing at all.
    for not_a_name in ["two words", "\"quoted\"", ""] {
        let refused = server.request(
            600,
            "textDocument/rename",
            json!({
                "textDocument": { "uri": main_uri },
                "position": at_the_use,
                "newName": not_a_name,
            }),
        );
        assert!(
            refused["result"].is_null(),
            "{not_a_name:?} is not a name: {refused}"
        );
    }

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **Expanding a selection walks a chain**, and the chain is the answer's *shape* rather than a list.
///
/// A client walks it outward: the word under the cursor, then each construct containing it, out to the file — and
/// every step has to be strictly larger, or the keystroke does nothing. Over the wire that is a nested object, each
/// range pointing at its parent, which is the part a client cannot reconstruct and therefore the part worth pinning.
#[test]
fn a_selection_expands_one_rung_at_a_time() {
    let project = Project::new("selection-range");
    project.write("main.cpp", SELECTION_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
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
    assert_eq!(
        capabilities["result"]["capabilities"]["selectionRangeProvider"],
        json!(true),
        "the client is told the server can expand a selection: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SELECTION_CPP }
        }),
    );

    // Line 1 is `    return w.size;` — the cursor is inside `size`, at the `i`.
    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/selectionRange",
            "params": {
                "textDocument": { "uri": main_uri },
                "positions": [{ "line": 1, "character": 13 }],
            },
        })
    });

    let answers = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("one answer per position was expected, got {answer}"));
    assert_eq!(answers.len(), 1, "one position, one chain: {answer}");

    // Walk the parent chain the way a client does, and record each rung's text by line and column.
    let mut rungs = Vec::new();
    let mut current = Some(&answers[0]);
    while let Some(range) = current {
        rungs.push((
            range["range"]["start"]["line"].as_u64().unwrap_or_default(),
            range["range"]["start"]["character"]
                .as_u64()
                .unwrap_or_default(),
            range["range"]["end"]["line"].as_u64().unwrap_or_default(),
            range["range"]["end"]["character"]
                .as_u64()
                .unwrap_or_default(),
        ));
        current = range.get("parent").filter(|parent| !parent.is_null());
    }

    // **The statement's rung ends at its `;`, on its own line.** It used to run to `(2, 0)` because the trailing
    // newline was glued inside the statement's node; the node now ends where its last token does, and the range a
    // client gets is the statement a reader would select — `return w.size;` and not the line break after it.
    //
    // **The body's rung begins one character early, at `(0, 7)` rather than `(0, 8)`**, and that is the trivia pass
    // rather than a slip: a block reaches back over the layout in front of it, because the layout between two
    // statements belongs to the block that holds both of them. Here the layout in front of `{` is the space in
    // `int f() {`, so the body's range covers it. The rule is the one the reference implementation states, and the
    // cost is a selection that starts one character left of the brace on the one line where a function's `{` is
    // written after a space.
    assert_eq!(
        rungs,
        vec![
            (1, 13, 1, 17), // `size`
            (1, 11, 1, 17), // `w.size`
            (1, 4, 1, 18),  // `return w.size;`
            (0, 7, 3, 0),   // the function body, including the space before its `{`
            (0, 0, 3, 0),   // the function definition
        ],
        "the chain, innermost first: {answer}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A symbol in a file nobody opened is found by a project search.**
///
/// Every other feature here is asked about a cursor in a file the client is showing; this one is asked about the
/// project, and the answer has to include declarations from files the user has never looked at — which is what the
/// index is for. The query is matched the way the index documents it (`Widget::si` finds the member, `ns::widget`
/// does not drag it in), and the one thing this test pins beyond that is the qualification travelling with the
/// symbol, because a client shows it beside the name.
#[test]
fn a_workspace_search_finds_a_symbol_in_a_file_nobody_opened() {
    let project = Project::new("workspace-symbol");
    project.write("search.h", SEARCH_H);
    project.write("main.cpp", SEARCH_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
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
    assert_eq!(
        capabilities["result"]["capabilities"]["workspaceSymbolProvider"],
        json!(true),
        "the client is told the server searches: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SEARCH_CPP }
        }),
    );

    // Asked again until it answers, because a search that arrives while the index still has work is refused
    // (`null`) rather than answered partially — see the handler's module note.
    let answer = server.ask_until_it(
        100,
        |id| json!({ "id": id, "method": "workspace/symbol", "params": { "query": "widget" } }),
        |response| {
            response["result"]
                .as_array()
                .is_some_and(|symbols| !symbols.is_empty())
        },
    );

    let found = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a symbol list was expected, got {answer}"));
    let names: Vec<&str> = found
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Widget", "WidgetRegistry"],
        "the name first, then the one that contains it: {answer}"
    );

    let widget = &found[0];
    assert!(
        widget["location"]["uri"]
            .as_str()
            .is_some_and(|uri| uri.ends_with("search.h")),
        "the class is declared in a file the client never opened: {widget}"
    );
    assert_eq!(
        widget["containerName"],
        json!("ns"),
        "and the namespace it is in travels with it: {widget}"
    );
    assert_eq!(widget["kind"], json!(5), "a class: {widget}");

    // The **qualified** query finds the same class, and the member only when it is asked for by its owner.
    let qualified = server.ask_until(
        200,
        |id| json!({ "id": id, "method": "workspace/symbol", "params": { "query": "Widget::si" } }),
    );
    let names: Vec<&str> = qualified["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["size"],
        "`Widget::si` asks for the member: {qualified}"
    );

    let class_only = server.ask_until(
        300,
        |id| json!({ "id": id, "method": "workspace/symbol", "params": { "query": "ns::widget" } }),
    );
    let names: Vec<&str> = class_only["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Widget"],
        "and a class query does not drag its members in: {class_only}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The outline shows the file, not the compiler** — both halves of that, in one fixture.
///
/// `#if OFF` is false once the header says so, so a compiler never sees `Dead` — and a reader editing that branch
/// very much does, which is why an outline is built from the file's **own** declarations rather than from the
/// cooked reading every other feature uses. The other direction is `DECLARE_HANDLE(HWND)`: the file writes a call,
/// so `HWND__` and `HWND` are in the index and in completions and are *not* here — an outline listing a name the
/// file never writes would be claiming something about the file that is not true.
#[test]
fn the_outline_shows_the_file_and_not_the_compiler() {
    let project = Project::new("outline");
    project.write("cfg.h", CFG_H);
    project.write("handle.h", HANDLE_H);
    project.write("main.cpp", OUTLINE_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let capabilities = server.request(
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
    assert_eq!(
        capabilities["result"]["capabilities"]["documentSymbolProvider"],
        json!(true),
        "the client is told the server outlines: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": OUTLINE_CPP }
        }),
    );

    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });

    let symbols = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a nested outline was expected, got {answer}"));

    // Every name in the tree, however deep, for the assertions about what is *not* there.
    fn all_names(symbols: &[Value], into: &mut Vec<String>) {
        for symbol in symbols {
            into.push(symbol["name"].as_str().unwrap_or_default().to_string());
            if let Some(children) = symbol["children"].as_array() {
                all_names(children, into);
            }
        }
    }

    let mut names = Vec::new();
    all_names(symbols, &mut names);
    assert_eq!(
        names,
        vec!["Dead", "x", "Live", "y"],
        "the branch nobody takes is in the outline, the file's own class, and nothing else: {answer}"
    );

    assert_eq!(symbols[0]["kind"], json!(5), "a class: {}", symbols[0]);
    assert_eq!(
        symbols[0]["children"][0]["kind"],
        json!(13),
        "a variable: {}",
        symbols[0]
    );

    // The pair of ranges: the name is what a client selects, and it is inside the declaration it folds.
    assert_eq!(
        symbols[0]["selectionRange"],
        json!({ "start": { "line": 3, "character": 7 }, "end": { "line": 3, "character": 11 } }),
        "`Dead` is selected by its own name: {}",
        symbols[0]
    );
    assert_eq!(
        symbols[0]["range"]["start"]["line"],
        json!(3),
        "and the declaration starts on the same line: {}",
        symbols[0]
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// An overloaded name answers with **every** location, which is the shape a client shows as a peek list.
///
/// One name covering several declarations is ordinary C++ — `find` in `std::basic_string` is seventeen of them in
/// MSVC's library — and `null` for all of them is a dead end for the reader. What must *not* happen is the other
/// mistake: a name with one declaration still answers `Scalar`, so nothing about the common case changes.
/// **The overload a call means is marked in the hover list.**
///
/// Hover on a name with several declarations answers with every signature, because that is what a reader on a callee is
/// asking. It used to say, in as many words, that *which* one the call means was not decided here — and now it is: the
/// resolver the checks and the signature popup share picks one, and the chosen line is the one with `→`.
/// **The chosen template overload is shown instantiated.**
///
/// `take(T* value)` is what the declaration says; `take(int* value)` is what this call means, and a reader who sees only
/// the first cannot tell that the analysis deduced anything. The substitution is whole-word, so `T` does not reach into
/// a longer name.
/// **The chosen template overload is shown instantiated** — `take(int* value)` rather than `take(T* value)`.
///
/// Two rules had to exist before this could pass: the template arguments are deduced from the call and substituted
/// (§12, §19), and two templates that deduce to the same types are settled by **partial ordering** — `T*` is more
/// specialized than `T` (§22). No `→` at all was the earlier measurement: without the names the candidates tie, and a
/// tie is `Ambiguous`.
#[test]
fn the_chosen_template_overload_is_shown_instantiated() {
    const TAKE_H: &str =
        "template <typename T> void take(T* value);\ntemplate <typename T> void take(T value);\n";
    const TAKE_CPP: &str =
        "#include \"take.h\"\nint g() { int n = 0; int* p = &n; take(p); return 0; }\n";

    let project = Project::new("hover-template-overloads");
    project.write("take.h", TAKE_H);
    project.write("main.cpp", TAKE_CPP);

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": TAKE_CPP }
        }),
    );

    let hovered = hover_over(&mut server, 100, &main_uri, TAKE_CPP, "take(p)");
    let text = hovered["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {hovered}"));

    assert!(
        text.contains("→ "),
        "an overload is marked as what the call means: {text}"
    );
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("→ ") && line.contains("int* value")),
        "and it is shown instantiated — `take(int* value)` rather than `take(T* value)`: {text}"
    );
}

/// **The overload a call means is marked in the list, and the mark is the argument's choice.**
///
/// **The overload a call means is the first location `textDocument/definition` answers.**
///
/// The protocol allows a list and a client shows it in order, so the first entry is what a reader who does not pick gets
/// — and for a call that should be the declaration its arguments select. The three declarations are one per line in the
/// header, so the line number of the first location says which one was chosen: `parse(1)` must lead with line 0,
/// `parse("x")` with line 1.
#[test]
fn the_definition_of_a_call_leads_with_the_overload_it_means() {
    const OVERLOADS_H: &str =
        "int parse(int value);\nint parse(const char* text);\nint parse(double value);\n";
    const OVERLOADS_CPP: &str = "#include \"overloads.h\"\nint f() { return parse(1); }\n";

    let project = Project::new("definition-overloads-order");
    project.write("overloads.h", OVERLOADS_H);
    project.write("main.cpp", OVERLOADS_CPP);

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": OVERLOADS_CPP }
        }),
    );

    // The cursor goes to `parse` in the call, and the request is repeated until the header has been read — the index is
    // lazy on purpose.
    let line = OVERLOADS_CPP
        .lines()
        .position(|line| line.contains("parse(1)"))
        .unwrap_or_default();
    let character = OVERLOADS_CPP
        .lines()
        .nth(line)
        .and_then(|text| text.find("parse"))
        .unwrap_or_default();

    let mut first_line = None;
    for attempt in 0..40 {
        let answer = server.request(
            100 + attempt,
            "textDocument/definition",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": line, "character": character },
            }),
        );
        let locations = answer["result"].as_array().cloned().unwrap_or_default();
        if locations.len() > 1 {
            first_line = locations[0]["range"]["start"]["line"].as_u64();
            break;
        }
    }

    assert_eq!(
        first_line,
        Some(0),
        "an `int` argument leads with `int parse(int value);`, which is line 0 of the header"
    );
}

#[test]
fn an_overload_set_answers_with_every_location() {
    const OVERLOADS_H: &str =
        "int parse(int value);\nint parse(const char* text);\nint parse(double value);\n";
    const OVERLOADS_CPP: &str = "#include \"overloads.h\"\nint f() { return parse(1); }\n";

    let project = Project::new("definition-overloads");
    project.write("overloads.h", OVERLOADS_H);
    project.write("main.cpp", OVERLOADS_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let header_uri = uri_of(&project.root().join("overloads.h"));

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": OVERLOADS_CPP }
        }),
    );

    // The cursor is inside `parse` on line 1 — `int f() { return parse(1); }` — and the request is repeated until
    // the header has been read, for the reason every query here is: the index is lazy on purpose.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/definition",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 1, "character": 20 },
                },
            })
        },
        |response| {
            response["result"]
                .as_array()
                .is_some_and(|locations| locations.len() == 3)
        },
    );

    let locations = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("three declarations, three locations: {answer}"));

    assert_eq!(locations.len(), 3);
    for (index, location) in locations.iter().enumerate() {
        assert_eq!(
            location["uri"],
            json!(header_uri),
            "all three are in the header: {location}"
        );
        assert_eq!(
            location["range"]["start"]["line"],
            json!(index),
            "one per line, in the order the header writes them: {answer}"
        );
    }

    // A name with **one** declaration is still `Scalar`: the common case does not change shape, which is what lets
    // a client that predates lists keep working.
    let single = server.request(
        200,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 1, "character": 4 },
        }),
    );
    assert!(
        single["result"]["uri"].is_string(),
        "`f` is declared on this line, and one declaration is one location: {single}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The call hierarchy over the wire**: prepare on a caller, then the two directions.
///
/// The three requests are a path through the dispatcher that no unit test covers — `prepareCallHierarchy` was asked on a
/// real project and the server **never answered**, so the plumbing is what this test is for.
#[test]
fn the_call_hierarchy_answers_over_the_wire() {
    let project = Project::new("call_hierarchy");
    project.write(
        "base.h",
        "int add(int a, int b);\nint subtract(int a, int b);\n",
    );
    let main = "#include \"base.h\"\n\
                int both(int a, int b) {\n    return add(a, b) + subtract(a, b);\n}\n";
    project.write("main.cpp", main);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "callHierarchy": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": main },
        }),
    );

    // **Inside the name**: `position_of` answers the position *after* the needle, so the needle ends at the `(` and two
    // characters back is the `h` of `both`.
    let (line, character) = position_of(main, "both(");
    let prepared = server.request(
        2,
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": line, "character": character - 2 },
        }),
    );
    println!("prepare: {prepared}");
    let item = prepared["result"]
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or_else(|| panic!("the function is a call hierarchy item: {prepared}"));
    assert_eq!(item["name"], "both");

    let incoming = server.request(3, "callHierarchy/incomingCalls", json!({ "item": item }));
    println!("incoming: {incoming}");
    assert!(
        !incoming["result"].is_null(),
        "the request is answered: {incoming}"
    );

    let outgoing = server.request(4, "callHierarchy/outgoingCalls", json!({ "item": item }));
    println!("outgoing: {outgoing}");
    let names: Vec<&str> = outgoing["result"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| call["to"]["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(names.contains(&"add"), "{names:?}");
    assert!(names.contains(&"subtract"), "{names:?}");
}

///
/// The count is the number of places that *use* the function: not its definition, not the declaration in the header.
/// Answered as `textDocument/codeLens`, so a client that asks gets one lens per function in the file.
#[test]
fn a_functions_reference_count_is_a_lens_over_the_wire() {
    const HEADER: &str = "int grow(int by);\n";
    const SOURCE: &str = "\
#include \"a.h\"
int grow(int by) { return by + 1; }
int f() { return grow(1) + grow(2); }
";
    let project = Project::new("code-lens");
    project.write("a.h", HEADER);
    project.write("a.cpp", SOURCE);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("a.cpp"));
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

    let lenses = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": uri } },
        })
    });
    let result = lenses["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a lens list was expected, got {lenses}"));
    assert!(
        result.iter().all(|lens| lens["command"].is_null()),
        "the first request returns anchors, not counts: {lenses}"
    );
    let titles: Vec<String> = result
        .iter()
        .enumerate()
        .filter_map(|(index, lens)| {
            let resolved = server.request(200 + index as i64, "codeLens/resolve", lens.clone());
            resolved["result"]["command"]["title"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert!(
        titles.iter().any(|title| title == "2 references"),
        "`grow` has two calls and neither its definition nor the header's declaration is counted: {titles:?}"
    );
}

/// **The type hierarchy over the wire**: the capability a client looks for, then prepare → supertypes → subtypes.
///
/// The capability is the part worth a protocol test rather than a unit one: `typeHierarchyProvider` is LSP 3.17 and
/// the `lsp_types` fork this server links against has no `ServerCapabilities` field for it, so the server writes it
/// into the `initialize` JSON by hand. A unit test of the handlers cannot see that field at all — a client that does
/// not, never asks, and the handlers are dead code however correct they are.
#[test]
fn a_client_can_walk_the_class_hierarchy() {
    let project = Project::new("type-hierarchy");
    let base = "struct Base {\n    virtual void run();\n};\n";
    let derived = "#include \"base.h\"\nstruct Derived : Base {\n    void run() override;\n};\n";
    project.write("base.h", base);
    project.write("derived.h", derived);

    let mut server = Server::start(project.root());
    let derived_uri = uri_of(&project.root().join("derived.h"));

    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "textDocument": { "typeHierarchy": { "dynamicRegistration": false } },
                "workspace": { "configuration": true },
            },
        }),
    );
    assert_eq!(
        initialized["result"]["capabilities"]["typeHierarchyProvider"],
        json!(true),
        "a client only asks if the server advertises it: {initialized}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": derived_uri, "languageId": "cpp", "version": 1, "text": derived }
        }),
    );

    // `Derived` is on line 1 (`struct Derived : Base`), at the name.
    let prepared = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/prepareTypeHierarchy",
            "params": {
                "textDocument": { "uri": derived_uri },
                "position": { "line": 1, "character": 7 },
            },
        })
    });
    let items = prepared["result"]
        .as_array()
        .unwrap_or_else(|| panic!("prepare answers an array of items: {prepared}"));
    assert_eq!(items.len(), 1, "one class under the cursor: {prepared}");
    let item = &items[0];
    assert_eq!(item["name"], json!("Derived"));
    assert_eq!(item["kind"], json!(5), "`SymbolKind::CLASS` is 5");
    assert!(
        item["data"]["file"].as_str().is_some(),
        "the item remembers where its class is: {prepared}"
    );

    let supertypes = server.request(200, "typeHierarchy/supertypes", json!({ "item": item }));
    let bases = supertypes["result"]
        .as_array()
        .unwrap_or_else(|| panic!("supertypes answers an array: {supertypes}"));
    assert_eq!(bases.len(), 1, "{supertypes}");
    assert_eq!(bases[0]["name"], json!("Base"));

    let subtypes = server.request(300, "typeHierarchy/subtypes", json!({ "item": bases[0] }));
    let classes = subtypes["result"]
        .as_array()
        .unwrap_or_else(|| panic!("subtypes answers an array: {subtypes}"));
    assert!(
        classes
            .iter()
            .any(|class| class["name"] == json!("Derived")),
        "`Base`'s subtypes name the class that derives from it: {subtypes}"
    );
}
