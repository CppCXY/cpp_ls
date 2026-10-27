//! **A workspace load must end the progress bar it began.**
//!
//! The bar is one promise — *"Load workspace"* — and it is kept when the queue has drained and the index can answer
//! questions about the project's files. It used to be owed until every translation unit had been read as a program;
//! that pass was deleted (`e22fa00`), the flag that carried the promise went with it, **and so did the `end`**, which
//! left the bar on screen for ever. Reported from an editor against a three-file CUDA project: `the workspace is
//! indexed` in the server's own log and no `end` on the wire.
//!
//! The fixture is that project, because it is the one that was reported — and because a CUDA workspace is a good
//! second subject for the same test: `.cu` files are only sources when the project is CUDA, and the grammar only
//! reads `k<<<g, b>>>(args)` when it is, so a bar that never ends here would hide both.
//!
//! What this asserts is the **wire**: `$/progress` begin, at least one report, and an end. A test that asked the
//! status bar would agree with the status bar.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long the workspace may take to load. Generous: a debug build asks the compiler for its search paths.
const TIMEOUT: Duration = Duration::from_secs(60);

fn uri_of(path: &Path) -> String {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    format!("file:///{}", text.trim_start_matches('/'))
}

fn read_messages(stdout: impl Read, tx: std::sync::mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut length = 0usize;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if line.trim().is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix("Content-Length:") {
                length = rest.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&body)
            && tx.send(value).is_err()
        {
            return;
        }
    }
}

#[test]
fn a_workspace_load_ends_its_progress_bar() {
    // **The reported project**, in miniature: a toolkit header, a kernel, a launch, and the configuration that says
    // the workspace is CUDA.
    let root = std::env::temp_dir().join("cppls-progress-cuda");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("the fixture directory");
    std::fs::create_dir_all(root.join("cuda/include")).expect("the fixture's toolkit");
    std::fs::write(
        root.join(".cppls.toml"),
        "[compile.cuda]\nenable = true\ntoolkit = \"cuda\"\n",
    )
    .expect("the fixture's configuration");
    std::fs::write(
        root.join("cuda/include/cuda_runtime.h"),
        "struct dim3 { unsigned x, y, z; };\nextern const dim3 threadIdx;\n",
    )
    .expect("the fixture's toolkit header");
    let source = "#include <cuda_runtime.h>\n\n__global__ void add(const float *a, float *out) {\n    unsigned i = threadIdx.x;\n    out[i] = a[i];\n}\n\nvoid host(const float *a, float *out) {\n    add<<<1, 128>>>(a, out);\n}\n";
    std::fs::write(root.join("src/kernels.cu"), source).expect("the fixture's kernel");

    let mut child = Command::new(env!("CARGO_BIN_EXE_cpp_ls"))
        .arg("--log-path")
        .arg("none")
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the server starts");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    // Drained, so that a full stderr pipe cannot park the server mid-test.
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            line.clear();
        }
    });
    let (tx, messages): (_, Receiver<Value>) = channel();
    std::thread::spawn(move || read_messages(stdout, tx));

    let mut send = |message: Value| {
        let body = serde_json::to_string(&message).expect("the message serializes");
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .expect("the message is written");
        stdin.flush().expect("the message is flushed");
    };
    send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootUri": uri_of(&root),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true }
            }
        }
    }));
    send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": uri_of(&root.join("src/kernels.cu")),
            "languageId": "cuda",
            "version": 1,
            "text": source
        }}
    }));

    let deadline = Instant::now() + TIMEOUT;
    let mut begun = 0usize;
    let mut ended = 0usize;
    while Instant::now() < deadline && !(begun > 0 && ended >= begun) {
        let Ok(message) = messages
            .recv_timeout(Duration::from_millis(250))
            .or_else(|error| match error {
                RecvTimeoutError::Timeout => Err(()),
                RecvTimeoutError::Disconnected => Err(()),
            })
            .map_err(|_| RecvTimeoutError::Timeout)
        else {
            continue;
        };

        // **Answer whatever the server asks**, as a client must — `workspace/configuration`,
        // `window/workDoneProgress/create`, `client/registerCapability`. A harness that ignores them measures a server
        // parked on a reply rather than the workspace.
        if let Some(id) = message.get("id") {
            let method = message["method"].as_str().unwrap_or("");
            let result = if method == "workspace/configuration" {
                let count = message["params"]["items"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or(0);
                json!(vec![Value::Null; count])
            } else {
                Value::Null
            };
            send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            continue;
        }

        if message["method"] != json!("$/progress") {
            continue;
        }
        match message["params"]["value"]["kind"].as_str().unwrap_or("") {
            "begin" => begun += 1,
            "end" => ended += 1,
            _ => {}
        }
    }

    let _ = child.kill();
    assert!(
        begun > 0,
        "the server said nothing about loading the workspace"
    );
    assert_eq!(
        begun, ended,
        "every progress bar must be retired: {begun} begun, {ended} ended"
    );
}
