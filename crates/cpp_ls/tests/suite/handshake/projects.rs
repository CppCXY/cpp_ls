//! Workspace shape: watches, reloads, modules, CUDA, Ascend and the CLI.

use super::fixtures::*;
use super::support::*;

/// **`shutdown` ends the process even when the client never sends `exit`.**
///
/// The protocol asks for `exit` after the shutdown answer, and a client that sends it gets to see the answer
/// first. A client that only says `shutdown` — and holds the pipe open, the way an editor does — is still a
/// client that expects the process to end: waiting for a notification that will not come is a server an editor
/// has to kill, which is what happened (the wait was thirty seconds, and the process outlived it).
#[test]
fn a_shutdown_ends_the_process_while_the_client_keeps_the_pipe() {
    let project = Project::new("shutdown-without-exit");
    project.write("main.cpp", "int main() { return 0; }\n");

    let mut server = Server::start(project.root());
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

    // Let the workspace come up, the way a client has been talking to the server for a while before it closes.
    server.pull_diagnostics(90, &uri_of(&project.root().join("main.cpp")));

    let shutdown = server.request(901, "shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);

    // **No `exit`, and `stdin` stays open** — the server holds it, so only its own code can end the process.
    assert!(
        server.wait_for_exit(),
        "the process must end after shutdown, not wait for an exit notification"
    );
}

#[test]
fn a_project_configuration_reaches_the_servers_behaviour() {
    let project = Project::new("configured");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);
    project.write(".cppls.toml", "[hover]\nenable = false\n");

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    // The same position the test above gets a declaration from — `Widget w;` on line 3.
    let hover = server.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 3, "character": 6 },
        }),
    );

    assert_eq!(
        hover["result"],
        Value::Null,
        "the project turned the popup off, and the server read the project's file to find that out: {hover}"
    );

    let shutdown = server.request(3, "shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    server.notify("exit", Value::Null);
    assert!(server.wait_for_exit());
}

/// **`[code_lens] enable = false` reaches the wire** — the lens is a preference a project can turn off, and the answer
/// is an empty list rather than no provider.
///
/// The capability cannot be withdrawn after `initialize`, so the switch is honoured where the lenses are made. The
/// same file is asked twice: once in a project without the setting (a lens for `f`), and once with it (nothing).
#[test]
fn a_project_can_turn_the_code_lens_off() {
    let source = "\
int grow(int by) { return by + 1; }
int f() { return grow(1) + grow(2); }
";
    for (name, config) in [
        ("code-lens-on", None),
        ("code-lens-off", Some("[code_lens]\nenable = false\n")),
    ] {
        let project = Project::new(name);
        project.write("main.cpp", source);
        if let Some(config) = config {
            project.write(".cppls.toml", config);
        }

        let mut server = Server::start(project.root());
        let main_uri = uri_of(&project.root().join("main.cpp"));
        let initialized = server.request(
            1,
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": uri_of(project.root()),
                "capabilities": { "textDocument": { "codeLens": { "dynamicRegistration": false } } },
            }),
        );
        assert!(
            initialized["result"]["capabilities"]["codeLensProvider"].is_object(),
            "the provider is advertised either way — a client asks once: {initialized}"
        );
        server.notify("initialized", json!({}));
        server.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": source }
            }),
        );
        server.pull_diagnostics(2, &main_uri);

        let lenses = server.request(
            3,
            "textDocument/codeLens",
            json!({ "textDocument": { "uri": main_uri } }),
        );
        let items = lenses["result"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: the answer is a list: {lenses}"));

        match config {
            None => {
                let titles: Vec<String> = items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, lens)| {
                        let resolved =
                            server.request(400 + index as i64, "codeLens/resolve", lens.clone());
                        resolved["result"]["command"]["title"]
                            .as_str()
                            .map(str::to_string)
                    })
                    .collect();
                assert_eq!(
                    titles,
                    vec!["2 references".to_string(), "0 references".to_string()],
                    "`grow` is called twice and `f` by nothing: {lenses}"
                );
            }
            Some(_) => assert!(
                items.is_empty(),
                "the project turned the lens off, so there is nothing to draw: {lenses}"
            ),
        }

        let shutdown = server.request(900, "shutdown", Value::Null);
        assert_eq!(shutdown["result"], Value::Null);
        server.notify("exit", Value::Null);
        assert!(server.wait_for_exit());
    }
}

#[test]
fn a_client_without_dynamic_watches_observes_an_external_header_edit() {
    let project = Project::new("native-watch-header");
    project.write("widget.h", FRESH_TYPE_H);
    project.write("main.cpp", FRESH_TYPE_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
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
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": FRESH_TYPE_CPP }
        }),
    );

    let (line, character) = position_of(FRESH_TYPE_CPP, "w.");
    let completion = |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": line, "character": character },
            },
        })
    };
    server.ask_until_it(100, completion, |response| {
        response["result"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["label"] == "size"))
    });

    project.write("widget.h", "struct Widget { int from_disk; };\n");
    let deadline = Instant::now() + TIMEOUT;
    let mut id = 200;
    loop {
        let request = completion(id);
        let response = server.request(id, "textDocument/completion", request["params"].clone());
        if response["result"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["label"] == "from_disk"))
        {
            break;
        }
        if let Some(error) = response.get("error") {
            assert_eq!(
                error["code"],
                json!(lsp_server::ErrorCode::ContentModified as i32)
            );
        }
        assert!(
            Instant::now() < deadline,
            "the external edit was not observed: {response}"
        );
        id += 1;
        std::thread::sleep(Duration::from_millis(50));
    }

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

#[test]
fn a_native_watcher_rescans_new_directories_after_a_burst() {
    let project = Project::new("native-watch-directory");
    project.write("initial.cpp", "int initial_symbol;\n");
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
    server.ask_until_it(
        10,
        |id| json!({ "id": id, "method": "workspace/symbol", "params": { "query": "initial_symbol" } }),
        |response| response["result"].as_array().is_some_and(|items| !items.is_empty()),
    );

    let directory = project.root().join("new_directory");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("new.cpp"), "int created_in_new_directory;\n").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let mut id = 100;
    loop {
        let response = server.request(
            id,
            "workspace/symbol",
            json!({ "query": "created_in_new_directory" }),
        );
        if response["result"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["name"] == "created_in_new_directory")
        }) {
            break;
        }
        if let Some(error) = response.get("error") {
            assert_eq!(
                error["code"],
                json!(lsp_server::ErrorCode::ContentModified as i32)
            );
        }
        assert!(
            Instant::now() < deadline,
            "the new directory was not indexed: {response}"
        );
        id += 1;
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_file_opened_without_a_workspace_gets_an_analysis_session() {
    let project = Project::new("single-file");
    let source = "struct Widget { int member; };\nvoid f() { Widget w; w.\n}\n";
    project.write("main.cpp", source);
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": null,
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
    let (line, character) = position_of(source, "w.");
    let answer = server.request(
        2,
        "textDocument/completion",
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
    );
    assert!(
        answer["result"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["label"] == "member")),
        "the single file must be analysed: {answer}"
    );
}

#[test]
fn a_configuration_reload_indexes_the_new_project_files() {
    let project = Project::new("reload-generation");
    project.write(".cppls.toml", "[workspace]\nsource_extensions = []\n");
    project.write("main.cpp", "int main() { return 0; }\n");
    project.write("extra.custom", "int ReloadedSymbol;\n");
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
    let before = server.request(2, "workspace/symbol", json!({ "query": "ReloadedSymbol" }));
    assert!(
        before["result"]
            .as_array()
            .is_none_or(|items| items.is_empty()),
        "the custom extension is not initially indexed: {before}"
    );

    project.write(
        ".cppls.toml",
        "[workspace]\nsource_extensions = [\"custom\"]\n",
    );
    server.notify(
        "workspace/didChangeWatchedFiles",
        json!({
            "changes": [{ "uri": uri_of(&project.root().join(".cppls.toml")), "type": 2 }]
        }),
    );
    std::thread::sleep(Duration::from_secs(3));
    server.ask_until_it(
        100,
        |id| json!({ "id": id, "method": "workspace/symbol", "params": { "query": "ReloadedSymbol" } }),
        |response| {
            response["result"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["name"] == "ReloadedSymbol"))
        },
    );
}
/// **A CUDA project, over the wire**: the completion list carries CUDA's vocabulary and the popup explains it.
///
/// The two features are one question asked twice — *is this project CUDA?* — and the answer decides both, so both are
/// asserted here against a project that says so in its own file. The vocabulary is not the language server's to
/// invent: `__global__` is an identifier to the lexer (a legal variable name in C++) and a specifier to `nvcc`, which
/// makes the *family* the switch rather than the spelling.
#[test]
fn a_cuda_project_completes_its_vocabulary_and_explains_it() {
    let project = Project::new("cuda-vocabulary");
    // **The project's own word about itself.** `enable` is what a machine without `nvcc` needs; the toolkit is not
    // installed here either, which is the point: `threadIdx` is answered from the table rather than from a header.
    project.write(".cppls.toml", "[compile.cuda]\nenable = true\n");
    let source = "__global__ void add(float *out) {\n    unsigned i = threadIdx.x;\n    __dev\n}\n";
    project.write("kernels.cu", source);
    let uri = uri_of(&project.root().join("kernels.cu"));

    let mut server = Server::start(project.root());
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "window": { "workDoneProgress": true } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cuda-cpp", "version": 1, "text": source }
        }),
    );

    // **The popup on a built-in variable**: asked until the index has read the file, because a hover that arrives
    // first is legitimately null. The needle stops *inside* the word — `position_of` answers the offset past it, and
    // past `threadIdx` is the `.`, whose token is punctuation and whose popup is nothing.
    let (line, character) = position_of(source, "threadId");
    let hover = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }
        })
    });
    let text = hover["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("threadIdx"),
        "the popup says what `threadIdx` is: {text:?}"
    );

    // **The completion list on a prefix**: `__dev` has one answer, and it is offered with its description.
    let (line, character) = position_of(source, "__dev");
    let completion = server.ask_until(200, |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": { "triggerKind": 1 }
            }
        })
    });
    let items = completion["result"]["items"]
        .as_array()
        .or_else(|| completion["result"].as_array())
        .expect("a completion list");
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();
    assert!(
        labels.contains(&"__device__"),
        "a CUDA project completes `__device__`: {labels:?}"
    );

    let shutdown = server.request(500, "shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    server.notify("exit", Value::Null);
    assert!(server.wait_for_exit());
}

/// **The other half of the switch**: the same word in a project that is not CUDA is a name, and nothing is offered.
///
/// No `.cppls.toml` and no `nvcc` in a database means `config.cuda()` is false, and then `__global__` declares nothing
/// — which is what a project that has never seen `nvcc` should be told.
#[test]
fn a_cpp_project_is_not_offered_cuda_vocabulary() {
    let project = Project::new("cuda-vocabulary-off");
    let source = "__global__ void add(float *out) { }\n";
    project.write("kernels.cu", source);
    let uri = uri_of(&project.root().join("kernels.cu"));

    let mut server = Server::start(project.root());
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "window": { "workDoneProgress": true } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );

    let (line, character) = position_of(source, "__glo");
    let completion = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": { "triggerKind": 1 }
            }
        })
    });
    let items = completion["result"]["items"]
        .as_array()
        .cloned()
        .or_else(|| completion["result"].as_array().cloned())
        .expect("a completion list");
    assert!(
        !items
            .iter()
            .any(|item| item["label"].as_str() == Some("__global__")),
        "a project that is not CUDA is not offered CUDA's vocabulary"
    );

    let shutdown = server.request(500, "shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    server.notify("exit", Value::Null);
    assert!(server.wait_for_exit());
}

/// **A configuration file saved from the editor reloads the project.**
///
/// The open-buffer rule — "the buffer is the text, the disk is not" — is right for source and wrong for the project's
/// own configuration, which is not edited into the analysis but **re-read**. `if open { continue }` skipped the reload,
/// and `didSave` carries no text (the server asks for none), so saving `.cppls.toml` in an editor changed nothing until
/// a restart (`docs/worklog.md` P1-4).
#[test]
fn a_configuration_file_saved_from_the_editor_reloads_the_project() {
    let project = Project::new("config-save-reload");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);
    project.write(".cppls.toml", "[hover]\nenable = false\n");
    let configuration = project.root().join(".cppls.toml");
    let configuration_uri = uri_of(&configuration);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "workspace": { "configuration": true } },
        }),
    );
    server.notify("initialized", json!({}));
    // The configuration file is **open in the editor**, which is the state a user saves it from.
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": configuration_uri,
                "languageId": "toml",
                "version": 1,
                "text": "[hover]\nenable = false\n",
            }
        }),
    );
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    let hover = |server: &mut Server, id: i64| {
        server.request(
            id,
            "textDocument/hover",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": 3, "character": 6 },
            }),
        )
    };
    assert_eq!(
        hover(&mut server, 2)["result"],
        Value::Null,
        "the project turned the popup off, and the server read that from the file"
    );

    // The user edits and saves: the disk gets the new text, then the notifications a save produces.
    std::fs::write(&configuration, "[hover]\nenable = true\n").expect("the configuration writes");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": configuration_uri, "version": 2 },
            "contentChanges": [{ "text": "[hover]\nenable = true\n" }],
        }),
    );
    server.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": configuration_uri } }),
    );
    server.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": configuration_uri, "type": 2 }] }),
    );

    // The reload is debounced and asynchronous: ask until the new configuration answers, within a bound.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut id = 10;
    loop {
        let answer = hover(&mut server, id);
        if !answer["result"].is_null() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("the saved configuration never reached the server: {answer}");
        }
        id += 1;
        std::thread::sleep(Duration::from_millis(100));
    }

    let _ = server.request(id + 1, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **The first `workspace/configuration` request names the workspace it is about.**
///
/// A folder-scoped setting can only be asked for under a `scopeUri`, and what a client answers without one is its global
/// setting — a different thing from the folder's. The handler asked for the client's configuration **before** installing
/// the roots, so the request went out with no `scopeUri` at all and a folder-level configuration was unreachable however
/// the client had set it (`docs/worklog.md` P1-1).
#[test]
fn the_first_configuration_request_names_the_workspace() {
    let project = Project::new("config-scope-uri");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);
    let root_uri = uri_of(project.root());

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": { "workspace": { "configuration": true } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    // **The synchronisation is the request itself**: the configuration ask happens inside `initialized` and the session
    // is opened after it, so an answer to anything queued behind that notification means the ask was answered — and
    // recorded by the client above.
    let _ = server.request(
        2,
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": main_uri } }),
    );

    let asked = server.configuration_requests.clone();
    assert!(
        !asked.is_empty(),
        "the client advertised `workspace.configuration`, so the server asked"
    );
    let scopes: Vec<String> = asked
        .iter()
        .flat_map(|params| params["items"].as_array().cloned().unwrap_or_default())
        .filter_map(|item| {
            item.get("scopeUri")
                .and_then(|scope| scope.as_str())
                .map(str::to_string)
        })
        .collect();
    assert!(
        scopes.iter().any(|scope| scope == &root_uri),
        "the request names the workspace root as its scope: {asked:?}"
    );

    let _ = server.request(3, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **A `--config` outside the workspace is watched, so editing it reloads the project.**
///
/// `--config` is kept on the workspace and used when the session opens, but the watch filter was built from the root
/// alone — the explicit path was not a configuration file to the server and, being outside the root, a recursive watch
/// could not see it. A configuration that is only read at startup is not a live one
/// (`docs/worklog.md` P1-5).
///
/// The client here advertises **no** dynamic registration, so the server's own watcher is the one that has to notice —
/// the path no client-side glob could mention either.
#[test]
fn a_configuration_file_outside_the_workspace_is_watched() {
    let project = Project::new("explicit-config");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);

    // The configuration lives **beside** the project, where nothing about the tree could predict it.
    let shared = project.root().parent().expect("a parent").join(format!(
        "{}-shared",
        project
            .root()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    std::fs::create_dir_all(&shared).expect("the shared directory");
    let configuration = shared.join("project-cppls.toml");
    std::fs::write(&configuration, "[hover]\nenable = false\n").expect("the configuration writes");
    let configuration = configuration.canonicalize().unwrap_or(configuration);
    let configuration_uri = uri_of(&configuration);

    let mut server = Server::start_with_args(
        project.root(),
        &[std::ffi::OsStr::new("--config"), configuration.as_os_str()],
    );
    let main_uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "workspace": { "configuration": true } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    let hover = |server: &mut Server, id: i64| {
        server.request(
            id,
            "textDocument/hover",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": 3, "character": 6 },
            }),
        )
    };
    assert_eq!(
        hover(&mut server, 2)["result"],
        Value::Null,
        "the explicit configuration turned the popup off, so the server read it"
    );

    // The user edits the shared configuration in another editor and saves it.
    std::fs::write(&configuration, "[hover]\nenable = true\n").expect("the configuration writes");
    server.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": configuration_uri, "type": 2 }] }),
    );

    // **The server's own watcher is the interesting path**, but a client that does report the change must reload too;
    // either way the popup comes back within the bound.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut id = 10;
    loop {
        let answer = hover(&mut server, id);
        if !answer["result"].is_null() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("the explicit configuration was never reloaded: {answer}");
        }
        id += 1;
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&shared);

    let _ = server.request(id + 1, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **`--editor` is the session's identity on both sides.**
///
/// Two computations of "which editor is this" disagreed under the argument: the capability side used
/// `initialize.clientInfo` and the configuration side used `--editor`, so a session started with `--editor vscode` from a
/// client that calls itself something else chose its *settings* as VS Code and its *commands* as that other client
/// (`docs/worklog.md` P2-2).
///
/// The observable is a code lens: VS Code is sent `cppls.showReferences` (its extension registers it, and it refuses the
/// editor's internal command), an IntelliJ client is sent `editor.action.showReferences`, and anything else gets no
/// command at all. The client here says it is Neovim and the argument says vscode, so the two identities would disagree
/// in one wire answer if both computations survived.
#[test]
fn the_editor_argument_is_the_identity_the_commands_are_chosen_with() {
    let source = "\
int grow(int by) { return by + 1; }
int f() { return grow(1); }
";
    let project = Project::new("editor-identity");
    project.write("main.cpp", source);

    let mut server = Server::start_with_args(
        project.root(),
        &[
            std::ffi::OsStr::new("--editor"),
            std::ffi::OsStr::new("vscode"),
        ],
    );
    let main_uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "clientInfo": { "name": "Neovim" },
            "capabilities": { "textDocument": { "codeLens": { "dynamicRegistration": false } } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );
    server.pull_diagnostics(2, &main_uri);

    let lenses = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": main_uri } },
        })
    });
    let listed = lenses["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a list of lenses: {lenses}"));
    assert!(
        !listed.is_empty(),
        "the fixture has a function to count: {lenses}"
    );
    let first = listed.first().expect("a lens").clone();
    let resolved = server.request(300, "codeLens/resolve", first);
    let command = resolved["result"]["command"]["command"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        command, "cppls.showReferences",
        "`--editor vscode` is the identity the command is chosen with, whatever the client calls itself: {resolved}"
    );

    let _ = server.request(200, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **The CLI does not advertise a switch nothing reads.**
///
/// `--load-stdlib` and `--resources-path` were declared and never consumed: a user could pass them and get no behaviour
/// change, which reads as "this is supported and did not help" rather than "this does nothing"
/// (`docs/worklog.md` P2-2). They are deleted, and the help text is the contract that says so — `--editor`,
/// which *is* read (the session's identity), stays listed.
#[test]
fn the_cli_does_not_offer_switches_nothing_reads() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
        .arg("--help")
        .output()
        .expect("the binary runs");
    let help = String::from_utf8_lossy(&output.stdout).to_string();

    for dead in ["load-stdlib", "resources-path"] {
        assert!(
            !help.contains(dead),
            "`--{dead}` is offered and nothing reads it: {help}"
        );
    }
    assert!(
        help.contains("--editor"),
        "the identity switch is offered, because it is read: {help}"
    );
}

/// **A request that follows `didChangeConfiguration` is answered after the new settings are in force.**
///
/// The notification used to be handled in a detached task: reading the client's configuration, updating the manager and
/// scheduling the reload all ran *beside* the update queue, so the next request could be answered from the configuration
/// that was in force before — and nothing told an in-flight request the world had changed
/// (`docs/worklog.md` P1-2).
///
/// The observable is the client's own configuration request: the test client counts those (it answers `[null]` to each),
/// so if the count has grown by the time the hover's answer comes back, the server finished the read **before** it
/// answered. As an update event it is guaranteed by the request barrier; as a detached task it was a race.
#[test]
fn a_request_after_a_configuration_change_sees_the_new_settings() {
    let project = Project::new("configuration-sequence");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);
    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "workspace": { "configuration": true } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    // One hover first, so the file is read and the answer below is about the sequence rather than the index.
    let warm = server.ask_until(10, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 3, "character": 6 },
            }
        })
    });
    assert!(
        !warm["result"].is_null(),
        "the fixture's `Widget` answers: {warm}"
    );

    let answered_before = server.configuration_answers;
    server.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": {} }),
    );
    // **The request by hand**, so this client can decide when to answer the settings: \Server::request\ answers every
    // server request on the way, which blurs exactly the distinction the test is about.
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 50,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": main_uri },
            "position": { "line": 3, "character": 6 },
        },
    }));

    // **While the settings are unanswered, the hover must not be.** The handler is reading them, and the request queues
    // behind that read; a server that dispatched the request from a detached task would answer it here.
    let early = server.read_holding_the_configuration(Duration::from_secs(3));
    assert!(
        early.iter().all(|message| message["id"] != json!(50)),
        "the request was answered before the settings it follows: {early:?}"
    );
    assert!(
        !server.held_configuration_requests.is_empty(),
        "the server did ask what the settings are now"
    );
    server.answer_the_held_configuration();

    let answered = server.wait_for(|message| message["id"] == json!(50));
    assert!(
        answered.get("error").is_none(),
        "the request is ordered behind the settings change rather than cancelled by it: {answered}"
    );
    assert!(
        server.configuration_answers > answered_before,
        "the new settings were **answered** before the request was, so it cannot have been served from the old ones: {:?}",
        server.configuration_requests
    );

    let _ = server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **A notification the server cannot read is said out loud, and the server keeps answering.**
///
/// The dispatcher dropped one silently — `if let Ok(params)` with no `else` — so a client bug or a field this server
/// reads and the client does not send left the analysis a document behind with nothing in the log to say why
/// (`docs/worklog.md` P2-6). The log is a file here (`--log-path`), which is also the only way a test can
/// see what a **notification** did: nothing is sent back to a client for one.
#[test]
fn a_malformed_notification_is_logged_and_does_not_stop_the_server() {
    let project = Project::new("malformed-notification");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);
    let logs = project.root().join("logs");

    let mut server = Server::start_with_args(
        project.root(),
        &[std::ffi::OsStr::new("--log-path"), logs.as_os_str()],
    );
    let main_uri = uri_of(&project.root().join("main.cpp"));
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

    // **Not a `didOpen`**: no `textDocument`, no `text`. The client is buggy, and the server has to say so rather than
    // act as though the document had opened.
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": main_uri } }),
    );

    // The server still answers, and the open it never processed is simply not there — the file is read from disk.
    let answered = server.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 3, "character": 6 },
        }),
    );
    assert!(
        answered.get("error").is_none(),
        "the malformed notification did not disturb the next request: {answered}"
    );

    let log = std::fs::read_dir(&logs)
        .expect("the log directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "log"))
        .expect("a log file");
    let written = std::fs::read_to_string(&log).expect("the log reads");
    assert!(
        written.contains("malformed textDocument/didOpen"),
        "the dropped notification is named, with its method: {written}"
    );
    assert!(
        written.contains(&main_uri.to_string()),
        "and with the URI the raw params still carried: {written}"
    );

    let _ = server.request(3, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **A client that refuses the watch registration is not left without a watcher.**
///
/// The registration was fire-and-forget: the response found no waiter and was dropped, so `MethodNotFound` looked like
/// agreement. The server then starts no watcher of its own — it believes the client reports changes — and nothing
/// reports anything, which is a session that silently stops noticing the filesystem
/// (`docs/worklog.md` P2-3).
///
/// The log is the observable: the refusal is named, and the native watcher starts in its place.
#[test]
fn a_refused_watch_registration_falls_back_to_the_native_watcher() {
    let project = Project::new("refused-registration");
    project.write("widget.h", WIDGET_H);
    project.write("main.cpp", MAIN_CPP);
    let logs = project.root().join("logs");

    let mut server = Server::start_with_args(
        project.root(),
        &[std::ffi::OsStr::new("--log-path"), logs.as_os_str()],
    );
    server.refuse_registrations = true;
    let main_uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    // A round trip, so the registration and the watcher decision have both happened.
    let _ = server.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 3, "character": 6 },
        }),
    );

    let log = std::fs::read_dir(&logs)
        .expect("the log directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "log"))
        .expect("a log file");
    let written = std::fs::read_to_string(&log).expect("the log reads");
    assert!(
        written.contains("refused to watch the project's files"),
        "the refusal is named rather than dropped: {written}"
    );
    assert!(
        written.contains("with the native file watcher"),
        "and the server watches the project itself: {written}"
    );

    let _ = server.request(3, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}
