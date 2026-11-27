//! Semantic tokens, inlay hints, signatures, folding, outlines, code lenses and formatting.

use super::fixtures::*;
use super::support::*;

/// The project's own `.cppls.toml` is read by the engine and honoured by the **server**.
///
/// The engine's tests prove the file is parsed; what they cannot prove is that a setting in it reaches the layer
/// that acts on it — the one file is parsed once, in the engine, and the server reads the same struct
/// (`Session::project_config`). Hover is the setting with an observable answer: with `hover.enable = false` the
/// same request that answers with a declaration in the test above answers `null`, and nothing else changes.
#[test]
fn a_formatting_request_returns_edits_for_the_whole_file() {
    let project = Project::new("formatting");
    // **This test names the built-in formatter**, because what it pins is *this server's* promise about a range —
    // that an edit never carries a line outside it — and an external clang-format's own `--lines` does not make that
    // promise (measured: 19.1.7 reindents the neighbour of the range). Which formatter a project runs is
    // `formatter_choice.rs`'s question; this one is the protocol's.
    project.write(".cppls.toml", "[format]\nformatter = \"cpp_formatter\"\n");
    // The file the server must be able to format: two spaces where one belongs, and a line indented with three.
    project.write("main.cpp", "int  x = 1;\n   int y = 2;\n");

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "formatting": { "dynamicRegistration": false } } },
        }),
    );
    let capabilities = &initialized["result"]["capabilities"];
    assert_eq!(
        capabilities["documentFormattingProvider"],
        json!(true),
        "the server advertises whole-file formatting: {capabilities}"
    );
    assert_eq!(capabilities["documentRangeFormattingProvider"], json!(true));

    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": main_uri,
                "languageId": "cpp",
                "version": 1,
                "text": "int  x = 1;\n   int y = 2;\n",
            }
        }),
    );

    let response = server.request(
        2,
        "textDocument/formatting",
        json!({
            "textDocument": { "uri": main_uri },
            "options": { "tabSize": 4, "insertSpaces": true },
        }),
    );

    let edits = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("edits were expected, got {response}"));
    assert!(
        !edits.is_empty(),
        "the file is not formatted, so there is work: {response}"
    );
    let text = edits
        .iter()
        .filter_map(|edit| edit["newText"].as_str())
        .collect::<String>();
    assert!(
        text.contains("int x = 1;"),
        "the double space between `int` and `x` is closed up: {response}"
    );
    assert!(
        !text.contains("   int y"),
        "and the three spaces before `int y` are gone: {response}"
    );

    // --- the same file, a range that covers only the first line ------------------------------------
    let response = server.request(
        3,
        "textDocument/rangeFormatting",
        json!({
            "textDocument": { "uri": main_uri },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } },
            "options": { "tabSize": 4, "insertSpaces": true },
        }),
    );
    let ranged = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a range answer is a list, possibly empty: {response}"));
    assert!(
        ranged.len() <= edits.len(),
        "a range never returns more than the whole file did: {response}"
    );
    // **A range must not touch a line outside it**: line 1 (`int y`) is not in the requested range, so no edit
    // may carry its text.
    let ranged_text = ranged
        .iter()
        .filter_map(|edit| edit["newText"].as_str())
        .collect::<String>();
    assert!(
        !ranged_text.contains("int y"),
        "the range formatted only line 0: {ranged_text:?}"
    );

    // --- willSaveWaitUntil: the edits the client applies before saving --------------------------------------
    let response = server.request(
        5,
        "textDocument/willSaveWaitUntil",
        json!({
            "textDocument": { "uri": main_uri },
            "reason": 1,
        }),
    );
    let edits = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("the save-time edits are a list: {response}"));
    assert!(
        !edits.is_empty(),
        "the file is still unformatted, so saving must offer the edits: {response}"
    );
}

/// **Folding is about the file, and the lines are the client's.**
///
/// Four regions, one of each kind the protocol names, and each one's line range is asserted exactly — because the
/// two ways this can be wrong are both off-by-one: an end taken past the region hides the line after it, and a
/// region whose ends land on one line hides nothing at all. The analysis answers in offsets and refuses the second
/// kind; the conversion to lines happens here, which is why this test goes over the wire rather than against the
/// analysis's own tests.
#[test]
fn the_folds_of_a_file_are_the_regions_a_reader_hides() {
    let project = Project::new("folding");
    project.write("main.cpp", FOLDS_CPP);

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
        capabilities["result"]["capabilities"]["foldingRangeProvider"],
        json!(true),
        "the client is told the server folds: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": FOLDS_CPP }
        }),
    );

    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/foldingRange",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });

    let ranges = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a folding range list was expected, got {answer}"));
    let seen: Vec<(u64, u64, Option<&str>)> = ranges
        .iter()
        .map(|range| {
            (
                range["startLine"].as_u64().unwrap_or_default(),
                range["endLine"].as_u64().unwrap_or_default(),
                range["kind"].as_str(),
            )
        })
        .collect();

    assert_eq!(
        seen,
        vec![
            (0, 1, Some("imports")),
            (3, 4, Some("comment")),
            (6, 8, None),
            (10, 12, Some("region")),
        ],
        "the include run, the comment block, the class (no kind: the protocol calls that code), the conditional"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **Semantic colours: each name is drawn as what declares it**, over the wire.
///
/// The answer is the protocol's delta walk — five numbers per token, the first two relative to the token before —
/// so the test decodes it back into absolute positions the way a client does, and asserts that a specific name is
/// drawn as a *specific legend entry*. That last part is the one a unit test cannot check: the numbers only mean
/// something against the legend the server advertised in `initialize`, and a legend that disagreed with the
/// encoding would colour every name as whatever sits at that index.
#[test]
fn semantic_colours_are_the_kinds_the_declarations_have() {
    let project = Project::new("semantic-tokens");
    project.write("main.cpp", COLOURS_CPP);

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
                "textDocument": {
                    "semanticTokens": {
                        "requests": { "full": true },
                        "tokenTypes": ["namespace", "type", "typeParameter", "enumMember", "function",
                                       "method", "variable", "parameter", "macro"],
                        "tokenModifiers": ["declaration"],
                        "formats": ["relative"],
                    },
                },
            },
        }),
    );
    let legend = capabilities["result"]["capabilities"]["semanticTokensProvider"]["legend"].clone();
    let index_of = |name: &str| -> u64 {
        legend["tokenTypes"]
            .as_array()
            .expect("a legend")
            .iter()
            .position(|kind| kind == name)
            .unwrap_or_else(|| panic!("the legend has no `{name}`: {legend}")) as u64
    };
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": COLOURS_CPP }
        }),
    );

    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/semanticTokens/full",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });

    let data = answer["result"]["data"]
        .as_array()
        .unwrap_or_else(|| panic!("a token array was expected, got {answer}"));
    let numbers: Vec<u64> = data
        .iter()
        .map(|value| value.as_u64().expect("the encoding is numbers"))
        .collect();
    assert_eq!(numbers.len() % 5, 0, "five numbers per token: {numbers:?}");

    // The client's own reconstruction: absolute line and character, then the length and the kind.
    let mut line = 0u64;
    let mut character = 0u64;
    let mut tokens: Vec<(u64, u64, u64, u64, u64)> = Vec::new();
    for token in numbers.chunks(5) {
        if token[0] == 0 {
            character += token[1];
        } else {
            line += token[0];
            character = token[1];
        }
        tokens.push((line, character, token[2], token[3], token[4]));
    }

    let at = |needle: &str| -> (u64, u64) {
        let line = COLOURS_CPP
            .lines()
            .position(|text| text.contains(needle))
            .expect("the fixture writes it") as u64;
        let character = COLOURS_CPP
            .lines()
            .nth(line as usize)
            .and_then(|text| text.find(needle))
            .expect("the fixture writes it") as u64;
        (line, character)
    };

    let coloured = |needle: &str| -> Option<(u64, u64, u64)> {
        let (line, character) = at(needle);
        tokens
            .iter()
            .find(|token| token.0 == line && token.1 == character)
            .map(|token| (token.2, token.3, token.4))
    };

    assert_eq!(
        coloured("LIMIT").map(|(_, kind, _)| kind),
        Some(index_of("macro")),
        "a macro this file defines: {tokens:?}"
    );
    assert_eq!(
        coloured("shapes").map(|(_, kind, _)| kind),
        Some(index_of("namespace")),
        "{tokens:?}"
    );
    assert_eq!(
        coloured("Widget").map(|(_, kind, _)| kind),
        Some(index_of("type")),
        "a class is a type here: {tokens:?}"
    );
    assert_eq!(
        coloured("size").map(|(_, kind, _)| kind),
        Some(index_of("variable")),
        "a field is a variable: {tokens:?}"
    );
    assert_eq!(
        coloured("scaled").map(|(_, kind, _)| kind),
        Some(index_of("method")),
        "a function in a class is a method: {tokens:?}"
    );
    assert_eq!(
        coloured("factor").map(|(_, kind, _)| kind),
        Some(index_of("parameter")),
        "and a parameter is not a variable: {tokens:?}"
    );

    // The length is the name's own, and the modifier says it is where the name is declared.
    let (length, _, modifiers) = coloured("LIMIT").expect("the macro is coloured");
    assert_eq!(length, "LIMIT".len() as u64);
    assert_eq!(modifiers, 1, "the declaration modifier: {tokens:?}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The signature popup, while the second argument is being typed.**
///
/// The cursor sits right after the comma — the state a signature exists for — and what has to come back is the
/// declaration's own text, the two parameter spans *inside that label*, the second one active, and the
/// documentation the file wrote above the declaration. A client bolds the active parameter by slicing the label
/// with the span, which is what this test does with it.
#[test]
fn a_signature_is_shown_while_the_arguments_are_typed() {
    let project = Project::new("signature-help");
    project.write("main.cpp", SIGNATURE_CPP);

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SIGNATURE_CPP }
        }),
    );

    let call_line = SIGNATURE_CPP
        .lines()
        .position(|line| line.contains("scale(1, )"))
        .expect("the fixture writes the call") as u64;
    let after_the_comma = SIGNATURE_CPP
        .lines()
        .nth(call_line as usize)
        .and_then(|line| line.find("scale(1, "))
        .expect("the fixture writes the call")
        + "scale(1, ".len();

    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": call_line, "character": after_the_comma },
            },
        })
    });

    let signature = &answer["result"]["signatures"][0];
    assert_eq!(
        signature["label"], "scale(int count, double factor)",
        "the declaration's own text: {answer}"
    );
    assert_eq!(answer["result"]["activeSignature"], json!(0));
    assert_eq!(
        answer["result"]["activeParameter"],
        json!(1),
        "the cursor is past the first comma, so the second parameter is active: {answer}"
    );

    // The parameter spans are offsets into the label — the client's own use of them, done here.
    let label = signature["label"].as_str().expect("a label");
    let spans: Vec<&str> = signature["parameters"]
        .as_array()
        .expect("the parameters are sent")
        .iter()
        .map(|parameter| {
            let offsets = parameter["label"].as_array().expect("offsets");
            let start = offsets[0].as_u64().expect("a start") as usize;
            let end = offsets[1].as_u64().expect("an end") as usize;
            &label[start..end]
        })
        .collect();
    assert_eq!(spans, vec!["int count", "double factor"]);

    // **The function's own text above the signature, and the `@param` line under the parameter it names.**
    //
    // Reported from an editor: "the function's popup should be synthesized from the doc and not include the
    // `@param` lines, and the parameters themselves should get their own". The whole comment shown twice said the
    // same sentence twice, the second time in the wrong place.
    let documentation = signature["documentation"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("the documentation above the declaration: {answer}"));
    assert!(documentation.contains("Scales a count."), "{documentation}");
    assert!(
        !documentation.contains("@param"),
        "the parameter's own sentence is not repeated above the signature: {documentation}"
    );

    let parameter_documentation = signature["parameters"][0]["documentation"]["value"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("`@param count the count` belongs to the first parameter: {answer}")
        });
    assert_eq!(parameter_documentation, "the count");
    assert_eq!(
        signature["parameters"][1]["documentation"],
        Value::Null,
        "a parameter with no `@param` line has no documentation rather than somebody else's: {answer}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The signature popup survives being inside a macro's argument.**
///
/// The same seam [`a_completion_inside_a_macros_argument_is_about_that_argument`] tests, asked by the other handler
/// that takes a cursor: signature help resolves the call around the offset out of the **rendering**, and the
/// rendering is where `PICK(1, scale(2, ))` has been rewritten into something with the call still in it but at a
/// different offset. A mapping that landed the cursor on the macro's own name, or on the first argument, would ask
/// about a different call — `pick_impl` here — and either answer nothing or answer with the wrong signature.
///
/// What has to come back is `scale`'s own label and its second parameter active, which is the state a reader sees
/// while typing the argument.
#[test]
fn a_signature_survives_being_inside_a_macro_argument() {
    let project = Project::new("signature-in-a-macro");
    project.write("main.cpp", SIGNATURE_IN_A_MACRO_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "signatureHelp": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SIGNATURE_IN_A_MACRO_CPP }
        }),
    );

    // `    return PICK(1, scale(2, ));` is line 6, and column 28 is right after the comma of `scale`'s arguments.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/signatureHelp",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 6, "character": 28 },
                },
            })
        },
        |answer| !answer["result"].is_null(),
    );

    let signatures = answer["result"]["signatures"]
        .as_array()
        .unwrap_or_else(|| panic!("a signature list was expected, got {answer}"));
    let labels: Vec<&str> = signatures
        .iter()
        .filter_map(|signature| signature["label"].as_str())
        .collect();
    assert!(
        labels.iter().any(|label| label.contains("scale")),
        "the signature of the call the cursor is in, not of the macro around it: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label.contains("PICK")),
        "and not the macro's own name: {labels:?}"
    );
    assert_eq!(
        answer["result"]["activeParameter"],
        json!(1),
        "the second argument is the one being typed: {answer}"
    );
}

/// **A C project's macro questions, asked the way the editor asks them** — the four a reader reported.
///
/// ```c
/// #ifdef __STDC_VERSION__                          hover: nothing at all
/// #define CORPUS_STANDARD __STDC_VERSION__         hover on the name: nothing at all
/// #endif
/// #define SUM(count, ...) (impl)(count __VA_OPT__(, ) __VA_ARGS__)
/// int use(void) { return SUM(total, 1, 2, 3); }    go to definition: nothing at all
/// _Static_assert(sizeof(int) >= 2, "…");           hover: nothing at all
/// ```
///
/// Each row is a question the *editor* asked and got nothing for. The analysis behind them is not missing — a macro's
/// `#define` is in the summary, and `_Static_assert` is a spelling the vocabulary can describe — so every failure here
/// is a handler that did not ask, and that is what this test measures. It runs against a real server over stdio because
/// the reported symptom is "no hover in the editor", and a session-level test cannot see a handler that never runs.
#[test]
fn the_c_macro_questions_the_editor_asks_are_answered() {
    let project = Project::new("macro-questions");
    project.write("macros.c", MACROS_C);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("macros.c"));
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
            "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": MACROS_C }
        }),
    );

    // **One shot per row**: the file is read by `prepare`, and a macro question is answered from that file's own
    // summary — a retry loop here would spin for the whole timeout on a row that is broken, and the first row's
    // failure is what this test exists to name.
    let hover_at = |server: &mut Server, id: i64, needle: &str| -> Value {
        let line = line_of(MACROS_C, needle);
        let character = MACROS_C
            .lines()
            .nth(line as usize)
            .and_then(|text| text.find(needle))
            .expect("the fixture writes it");
        server.request(
            id,
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
    };

    // **A macro the compiler predefines**, in a directive's condition.
    let predefined = hover_at(&mut server, 100, "__STDC_VERSION__");
    let text = predefined["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("`__STDC_VERSION__` has a hover: {predefined}"));
    assert!(
        text.contains("predefined") && text.contains("__STDC_VERSION__"),
        "the compiler defines it, and the card says so: {text}"
    );

    // **A macro at its own definition.**
    let definition = hover_at(&mut server, 200, "CORPUS_STANDARD");
    let text = definition["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("`CORPUS_STANDARD` has a hover at its definition: {definition}"));
    assert!(
        text.contains("CORPUS_STANDARD"),
        "the card names the macro: {text}"
    );

    // **`_Static_assert`,** which is a contextual spelling rather than a keyword.
    let assertion = hover_at(&mut server, 300, "_Static_assert");
    let text = assertion["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("`_Static_assert` has a hover: {assertion}"));
    assert!(
        text.contains("_Static_assert") && text.contains("compile time"),
        "the card explains the construct: {text}"
    );

    // **Go to definition on a macro use**, which is a *translation order* question rather than a name lookup.
    let line = line_of(MACROS_C, "SUM(selected, 1, 2, 3)");
    let character = MACROS_C
        .lines()
        .nth(line as usize)
        .and_then(|text| text.find("SUM"))
        .expect("the fixture writes the call");
    let jumped = server.ask_until(400, |id| {
        json!({
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            },
        })
    });
    let location = if jumped["result"].is_array() {
        jumped["result"][0].clone()
    } else {
        jumped["result"].clone()
    };
    assert_eq!(
        location["uri"].as_str().map(|found| found == uri),
        Some(true),
        "a macro's definition is the `#define` in this file: {jumped}"
    );
    assert_eq!(
        location["range"]["start"]["line"].as_u64(),
        Some(line_of(MACROS_C, "#define SUM(")),
        "and the line it names is the directive's: {jumped}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

#[test]
fn a_declaration_a_macro_makes_is_found_over_the_wire() {
    let project = Project::new("macro-declared");
    project.write("handle.h", HANDLE_H);
    project.write("api.h", API_H);
    project.write("main.cpp", API_CPP);

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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": API_CPP }
        }),
    );

    // `HWND h;` is the second line, and the name starts it.
    let location = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 1, "character": 1 },
            },
        })
    });

    let found = &location["result"];
    let found_uri = found["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("a definition was expected, got {location}"));
    assert!(
        found_uri.ends_with("api.h"),
        "the declaration the macro made belongs to the file that invoked it, which nobody opened: {location}"
    );
    assert_eq!(
        found["range"]["start"]["line"],
        json!(1),
        "and it is reported at the invocation: {location}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A name only the cooked reading declares is offered.**
///
/// `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND` to a compiler and *nothing* to a reader of `api.h`: the
/// declaration is the replacement list. So a completion in `main.cpp` that offers `HWND__` is one this server can
/// give only because the file it is declared in was read a **second** way — and the falsification (excluding the
/// cooked declarations from the index) turns this test red, which is what makes it evidence rather than a fixture
/// that happens to work.
///
/// The **edit** is asserted as well as the label: the cursor is inside a half-written name, so what comes back is
/// the range of that name to replace, not a point to insert at.
#[test]
fn a_name_only_the_cooked_reading_declares_is_offered_over_the_wire() {
    let project = Project::new("completion-macro-declared");
    project.write("handle.h", HANDLE_H);
    project.write("api.h", API_H);
    project.write("main.cpp", HWND_CPP);

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
    assert!(
        capabilities["result"]["capabilities"]["completionProvider"].is_object(),
        "the client is told the server completes: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": HWND_CPP }
        }),
    );

    // Line 2 is `    HWND` — the cursor is at its end. Asked again until the list holds `HWND__`, because a
    // completion answered before the project has been read is an empty list that says `isIncomplete`.
    let answer = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": main_uri },
                    "position": { "line": 2, "character": 8 },
                },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("HWND__")))
        },
    );

    let items = answer["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a completion list was expected, got {answer}"));
    assert!(
        items.iter().any(|item| item["label"] == json!("HWND")),
        "and the typedef the same macro made is offered beside it: {answer}"
    );

    let generated = items
        .iter()
        .find(|item| item["label"] == json!("HWND__"))
        .expect("the closure accepted this");
    assert_eq!(generated["kind"], json!(7), "a class: {generated}");
    assert_eq!(
        generated["textEdit"]["range"]["start"],
        json!({ "line": 2, "character": 4 }),
        "the edit replaces the half-written name from where it starts: {generated}"
    );
    assert_eq!(
        generated["textEdit"]["range"]["end"],
        json!({ "line": 2, "character": 8 }),
        "…to where the cursor is: {generated}"
    );
    assert_eq!(
        generated["textEdit"]["newText"],
        json!("HWND__"),
        "{generated}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A parameter name is drawn at the argument — and not where the argument already says it.**
///
/// **The hints a person is actually looking at**: asked one turn after an edit, with the queue still busy.
///
/// This is the case that made the feature look broken, and the report was exact: *"as soon as I have edited the code
/// the inlay hints stop appearing"*. The handler answered `null` — not an empty list, a refusal — while
/// `Session::pending() > 0`, on the reasoning that a callee in a file nobody has read yet resolves to nothing. The
/// reasoning is right about the cause and wrong about the remedy: `parameter_hints` **skips** an unresolved callee,
/// so a filling index yields fewer hints and never a wrong one.
///
/// What that cost, measured on a 31-line file with four `#include`s: an edit leaves the queue busy for **~48 ms**,
/// question about the same file returns **4 hints on the file's own tokens with the index complete** — so the
/// reading was never the problem and the refusal was the whole of it.
/// two apart deliberately; it was removed with the rest of the scaffolding once the cause was found.
/// question about the same file returns **4 hints on the file's own tokens with the index complete** — so the
/// reading was never the problem and the refusal was the whole of it.
///
/// The ask here waits for **the first answer that is not a refusal**, and then edits and asks **once**, with no
/// retry: a retry would let the queue drain and measure the wrong thing.
///
/// # What this test does not prove, recorded because it was tried
///
/// It **passes with the refusal put back**. The notification and the request are two messages and the server drains
/// in between — the window is ~48 ms, and this test does not control the race. The instrument that does show the
/// in between — the window is ~48 ms, and this test does not control the race. The instrument that did show it
/// was a probe that held the two apart deliberately, and it was removed with the rest of the scaffolding.
/// reports the depth and the count together.
///
/// And even there one number is ambiguous, which is worth knowing before trusting it: for a fixture whose callees
/// are declared **in the edited file**, the analysis itself answers nothing until the drain, because the edit
/// dropped that file's summary — so "the refusal" and "the honest empty answer" read the same. The reported file is
/// the other shape: its calls are into headers (`std::format`, `printf`), whose declarations the edit did not
/// touch, so the analysis had the answer all along and only the refusal was hiding it.
#[test]
fn the_hints_survive_the_edit_that_asks_for_them() {
    let project = Project::new("inlay-hints-after-an-edit");
    project.write("widget.h", INLAY_HEADER);
    project.write("main.cpp", INLAY_ACROSS_FILES);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let whole = json!({
        "start": { "line": 0, "character": 0 },
        "end": { "line": INLAY_ACROSS_FILES.lines().count(), "character": 0 },
    });

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "inlayHint": {} } },
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
                "text": INLAY_ACROSS_FILES,
            }
        }),
    );

    // Settled first, so that what is measured below is the **edit** and not the initial index.
    let settled = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/inlayHint",
                "params": { "textDocument": { "uri": main_uri }, "range": whole },
            })
        },
        |answer| {
            answer["result"]
                .as_array()
                .is_some_and(|hints| !hints.is_empty())
        },
    );
    assert!(
        settled["result"]
            .as_array()
            .is_some_and(|hints| hints.len() == 1),
        "one argument that does not spell its parameter: {settled}"
    );

    // **One keystroke**, in a function body: no directive moves, so this is the cheapest edit there is — and it is
    // the one whose hint request used to be refused. The callee is in `widget.h`, which the edit does not touch, so
    // the analysis can answer immediately and nothing but a refusal can be suppressing it.
    let edited = INLAY_ACROSS_FILES.replace("return scale(count, 1);", "return scale(count, 1) ;");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": main_uri, "version": 2 },
            "contentChanges": [{ "text": edited }],
        }),
    );

    let after = server.request(
        200,
        "textDocument/inlayHint",
        json!({ "textDocument": { "uri": main_uri }, "range": whole }),
    );
    let hints = after["result"]
        .as_array()
        .unwrap_or_else(|| panic!("**the hints were refused outright after an edit**: {after}"));
    assert_eq!(
        hints.len(),
        1,
        "an edit that moves no directive changes nothing a hint depends on, and the callee is in a header: {after}"
    );
}

/// Three arguments, two hints: `scale(count, …)` needs no `count:` in front of `count`, `1` is the second
/// parameter however it is spelled, and the member call's argument is named by the *member's* declaration rather
/// than by anything in the calling file.
///
/// The `ask_until` below is about the **index**, not about a refusal: this request is made before the workspace has
/// been read at all, when the callee genuinely has no declaration to take parameter names from, and the answer is an
/// empty list. It is the loop that gets it past that moment — see
/// [`the_hints_survive_the_edit_that_asks_for_them`] for the case that used to be answered with `null` instead.
#[test]
fn parameter_names_are_drawn_at_the_arguments() {
    let project = Project::new("inlay-hints");
    project.write("main.cpp", INLAY_CPP);

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
        capabilities["result"]["capabilities"]["inlayHintProvider"]["resolveProvider"],
        json!(false),
        "the client is told the server hints, and that there is nothing to resolve: {capabilities}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": INLAY_CPP }
        }),
    );

    let call_line = line_of(INLAY_CPP, "return scale(count, 1)");
    let answer = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/inlayHint",
            "params": {
                "textDocument": { "uri": main_uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": INLAY_CPP.lines().count(), "character": 0 },
                },
            },
        })
    });

    let hints = answer["result"]
        .as_array()
        .unwrap_or_else(|| panic!("an inlay hint list was expected, got {answer}"));
    let seen: Vec<(u64, String)> = hints
        .iter()
        .map(|hint| {
            (
                hint["position"]["line"].as_u64().expect("a line"),
                hint["label"].as_str().expect("a label").to_string(),
            )
        })
        .collect();

    assert_eq!(
        seen,
        vec![
            (call_line, "factor:".to_string()),
            (call_line, "scale:".to_string()),
        ],
        "`count` spells its parameter and gets no hint; the other two do: {answer}"
    );

    // The position is on the argument itself and the padding is after the label, so a client draws `factor: 1`
    // rather than `factor :1`.
    let factor = &hints[0];
    let argument = INLAY_CPP
        .lines()
        .nth(call_line as usize)
        .and_then(|line| line.find("scale(count, 1)"))
        .expect("the fixture writes the call")
        + "scale(count, ".len();
    assert_eq!(
        factor["position"]["character"],
        json!(argument),
        "the hint sits on the argument: {factor}"
    );
    assert_eq!(factor["paddingRight"], json!(true), "{factor}");
    assert_eq!(factor["kind"], json!(2), "a parameter hint: {factor}");

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}
