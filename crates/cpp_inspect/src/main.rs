//! # `cpp_inspect` — one question about one project, answered as JSON
//!
//! ```text
//! cpp_inspect --project C:\…\test_cpp --file src/templates.cpp type --needle "Pair<int, int>{1, 2}.sum" --shift 20
//! cpp_inspect --project C:\…\test_cpp --file src/smart_pointers.cpp hover --needle "node->twin" --text
//! cpp_inspect --fixture "a.cpp=struct Widget { int size() const { return 1; } };" --needle size hover
//! cpp_inspect caps
//! ```
//!
//! The questions are in [`questions`], the position one is asked at is [`place::Place`], and the session it is asked of
//! is [`project`]. `crates/cpp_inspect/README.md` has the protocol and the recipes.
//!
//! # Why a binary and not a test
//!
//! Because a question costs a **run** instead of a **build**. Every answer here used to be a `tests/tmp_*.rs` file:
//! write it, compile it, read four lines, delete it — and the compile dominated. The analysis is the same either way;
//! what changes is that the question is an argument now, and the next question does not need a new crate target.
//!
//! # Why JSON by default
//!
//! The first client is an agent, and an agent parses. A stable envelope (`schema`, `ok`, `command`, `query`, `answer`)
//! is cheaper to consume than a text format that has to be re-learned per command, and `--text` keeps the old
//! rendering for a person. `caps` is the machine-readable list of commands — a tool nobody can discover is a tool
//! nobody uses — and `batch` is what makes a *series* of questions affordable: one process, one index, one answer per
//! line.

mod caps;
mod place;
mod project;
mod questions;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use place::Place;
use project::ProjectArgs;
use questions::{Answer, FactFilters};

/// **The protocol's version.** A consumer may key its parsing on it; it moves when the envelope's shape does, not when
/// a command is added.
pub const SCHEMA: u32 = 1;

#[derive(Parser)]
#[command(
    name = "cpp_inspect",
    about = "Ask the analysis one question about one project, as JSON",
    version,
    after_help = "A position is `--at <byte>`, `--needle <text>` (+ `--occurrence`/`--last`/`--shift`), or \
                  `--line`/`--col`. `cpp_inspect caps` lists every command as JSON. `cpp_inspect batch` answers \
                  newline-delimited JSON requests on stdin."
)]
struct Cli {
    #[command(flatten)]
    project: ProjectArgs,
    #[command(flatten)]
    place: Place,
    /// Print the human rendering instead of JSON.
    #[arg(long, global = true)]
    text: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Clone)]
enum Command {
    /// The type of the expression at the position.
    Type,
    /// The hover card the server would show at the position.
    Hover,
    /// The declarations of the name at the position: the one the analysis picks, and all of them.
    Define,
    /// The members of a class, with the class that declares each and how many base steps away.
    Members {
        /// The class, as an editor would write it: `std::shared_ptr<int>`, `Widget`.
        #[arg(long)]
        class: String,
        /// How many to print; 0 is all of them.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    /// The macro in force for the name at the position.
    Macro,
    /// The completion labels offered at the position.
    Complete {
        /// How many to print; 0 is all of them.
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// The facts a file's summary holds, filtered by name, kind or scope.
    Facts {
        /// Only facts whose name contains this.
        #[arg(long)]
        name: Option<String>,
        /// Only this kind: `type`, `function`, `variable`, `namespace`, `macro-like`, `other`.
        #[arg(long)]
        kind: Option<String>,
        /// Only facts whose scope contains this.
        #[arg(long)]
        scope: Option<String>,
        /// How many to print; 0 is all of them.
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// The inlay hints an editor would draw for the file: parameter names and deduced types.
    Inlay,
    /// The findings a diagnostics channel would report for the file.
    Diagnostics,
    /// The summary's key, includes and counts.
    Summary,
    /// The syntax tree at the position, innermost node first.
    Tree {
        /// How many ancestors to print; 0 is all of them.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// The nodes of the syntax tree whose text contains this pattern — the whole tree, not a position.
    Grep {
        /// What the node's text must contain.
        pattern: String,
        /// How many nodes to print.
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// The regions of the file this configuration does not compile — what an editor greys out.
    Inactive,
    /// The reading format the index on disk uses.
    Format,
    /// The session's own counters after the setup.
    Stats,
    /// **The cooked text** — what the parser reads: includes inlined, comments and line breaks gone, macros expanded.
    ///
    /// The one answer that makes a cook defect diagnosable, since a syntax error is reported against this text and
    /// this text is not the file a reader has open.
    Rendering {
        /// A byte offset to slice from.
        #[arg(long)]
        at: Option<usize>,
        /// How many bytes to answer; 0 is all of them.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    /// **A whole project's findings, tallied** — every file cooked and diagnosed, grouped by code.
    ///
    /// The question the deleted probe tests answered by hand; `--all-files` is implied, because a sweep of one file is
    /// what `diagnostics` already is.
    Sweep {
        /// Only these codes (`undefined-name`, `syntax-error`, …). Repeatable.
        #[arg(long = "code")]
        codes: Vec<String>,
        /// Only files whose path contains this.
        #[arg(long)]
        including: Option<String>,
        /// How many findings to list per file.
        #[arg(long, default_value_t = 12)]
        limit: usize,
    },
    /// What this tool can answer, as JSON.
    Caps,
    /// Read newline-delimited JSON requests on stdin and answer each on one line of stdout.
    Batch,
}

fn main() {
    let cli = Cli::parse();
    // **A worker thread with a large stack.** The analysis walks syntax trees and include graphs recursively, and a real
    // translation unit is deeper than a debug build's 1 MB main-thread stack survives — measured: `type` on
    // `templates.cpp` overflowed it. A release build does not, which is exactly the kind of difference a tool must not
    // have between two ways of running it.
    const THE_STACK: usize = 64 << 20;
    let worker = std::thread::Builder::new()
        .name("cpp_inspect".to_string())
        .stack_size(THE_STACK)
        .spawn(move || run(cli))
        .expect("a thread with a stack");
    match worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            // **The failure is JSON too**, because a consumer's parser should not have two shapes to handle. The exit
            // code still says what happened, for a shell.
            println!("{}", failure(&error));
            std::process::exit(2);
        }
        Err(_) => {
            eprintln!("cpp_inspect: the worker panicked");
            std::process::exit(3);
        }
    }
}

fn run(mut cli: Cli) -> Result<(), String> {
    // **A sweep is about the project**, so the two flags that make the session hold the whole project are implied:
    // a sweep of one file is what `diagnostics` already is, and a caller that has to remember `--index --all-files`
    // for the one command whose meaning is "all of them" is a caller the protocol failed.
    if matches!(
        cli.command,
        Command::Sweep { .. } | Command::Rendering { .. }
    ) || cli.project.cook
    {
        // **Every file under the root is *seeded*, not indexed up front.** `index` would make the pump build a
        // stored cooked reading for each file, and `Session::diagnostics` prefers a stored reading over the one this
        // command cooks — which is the *index's* reading rather than the one a client's pull gets. Measured on
        // nlohmann/json (a header-only project): the stored reading had no `-I` for `tests/thirdparty/doctest`, so
        // `CHECK_THROWS_WITH_AS(…, json::out_of_range&)` stayed unexpanded and ten syntax errors appeared per test
        // file, where `diagnostics --cook` on the same file answered none.
        cli.project.all_files = true;
    }
    // **A debug build answers the same questions, minutes later.** The analysis is tree walks and inference; optimised
    // out, a project whose closure reaches the standard library takes minutes instead of a second — measured: `type` on
    // `test_cpp` did not finish in ten minutes in a debug build and answered in under a second in a release one. The
    // warning is printed rather than the tool refusing, because a small fixture is fine in debug and says so by
    // finishing. It goes to stderr so that stdout stays one JSON object per answer.
    if cfg!(debug_assertions) {
        eprintln!(
            "[cpp_inspect] a debug build: answers that index a real project can take minutes. `cargo run --release -p cpp_inspect -- …` is the same tool at production speed."
        );
    }

    match &cli.command {
        Command::Caps => {
            print_answer(
                &Answer::new(String::new(), caps::the_capabilities()),
                cli.text,
                "caps",
            )?;
            return Ok(());
        }
        Command::Batch => return batch(&cli),
        _ => {}
    }

    let mut opened = project::open(&cli.project)?;
    let answer = answer_for(&mut opened, &cli.command, &cli.place)?;
    print_answer(&answer, cli.text, command_name(&cli.command))
}

/// **The answer to one command** — the one place the dispatch lives, so a single question and a batched one cannot
/// answer differently.
fn answer_for(
    opened: &mut project::Opened,
    command: &Command,
    place: &Place,
) -> Result<Answer, String> {
    match command {
        Command::Type => questions::the_type(&*opened, place),
        Command::Hover => questions::the_hover(&*opened, place),
        Command::Define => questions::what_it_names(&*opened, place),
        Command::Members { class, limit } => questions::the_members(&*opened, class, *limit),
        Command::Macro => questions::the_macro(&*opened, place),
        Command::Complete { limit } => questions::the_completions(&*opened, place, *limit),
        Command::Facts {
            name,
            kind,
            scope,
            limit,
        } => questions::the_facts(
            &*opened,
            &FactFilters {
                name: name.as_deref(),
                kind: kind.as_deref(),
                scope: scope.as_deref(),
                limit: *limit,
            },
        ),
        Command::Inlay => questions::the_inlay(&*opened),
        Command::Diagnostics => questions::the_diagnostics(&*opened),
        Command::Summary => questions::the_summary(&*opened),
        Command::Tree { limit } => questions::the_tree(&*opened, place, *limit),
        Command::Grep { pattern, limit } => questions::the_grep(&*opened, pattern, *limit),
        Command::Inactive => questions::the_inactive(opened),
        Command::Format => Ok(questions::the_format()),
        // **The stats answer a `String`**, because it is a report about the run rather than a fact about the code.
        // Wrapped here so every command answers the same `Answer`.
        Command::Stats => {
            let text = questions::the_stats(opened);
            Ok(Answer::new(text.clone(), json!({ "text": text })))
        }
        Command::Rendering { at, limit } => Ok(questions::the_rendering(opened, *at, *limit)),
        Command::Sweep {
            codes,
            including,
            limit,
        } => Ok(questions::the_sweep(
            opened,
            codes,
            including.as_deref(),
            *limit,
        )),
        Command::Caps | Command::Batch => unreachable!("handled by the caller"),
    }
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Type => "type",
        Command::Hover => "hover",
        Command::Define => "define",
        Command::Members { .. } => "members",
        Command::Macro => "macro",
        Command::Complete { .. } => "complete",
        Command::Facts { .. } => "facts",
        Command::Inlay => "inlay",
        Command::Diagnostics => "diagnostics",
        Command::Summary => "summary",
        Command::Tree { .. } => "tree",
        Command::Grep { .. } => "grep",
        Command::Inactive => "inactive",
        Command::Format => "format",
        Command::Stats => "stats",
        Command::Rendering { .. } => "rendering",
        Command::Sweep { .. } => "sweep",
        Command::Caps => "caps",
        Command::Batch => "batch",
    }
}

/// One answer, as the protocol's envelope: `schema`, `ok`, `command`, `query`, `answer`.
fn print_answer(answer: &Answer, text: bool, command: &str) -> Result<(), String> {
    if text {
        print!("{}", answer.text);
        if !answer.text.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    let envelope = json!({
        "schema": SCHEMA,
        "ok": true,
        "command": command,
        "answer": answer.value,
    });
    println!(
        "{}",
        serde_json::to_string(&envelope).map_err(|error| error.to_string())?
    );
    Ok(())
}

/// The envelope a failure gets — the same keys, with `ok` false.
fn failure(error: &str) -> String {
    serde_json::to_string(&json!({
        "schema": SCHEMA,
        "ok": false,
        "error": { "code": "failed", "message": error },
    }))
    .unwrap_or_else(|_| "{\"schema\":1,\"ok\":false,\"error\":{\"code\":\"failed\"}}".to_string())
}

/// **Many questions, one process** — newline-delimited JSON on stdin, one answer per line on stdout.
///
/// Each line is a request in the same vocabulary as the command line: `{"command":"hover","file":"a.h","at":42}`. It
/// is turned into an argument list and parsed by the same `clap` grammar the command line uses, so every flag, default
/// and error message is the same one — a second parser for batch requests would be a second protocol to keep in step.
///
/// The session is **reused while the project and file stay the same**, which is the whole reason this command exists:
/// opening, indexing and cooking a project is the cost, and a series of questions about one file should pay it once.
fn batch(cli: &Cli) -> Result<(), String> {
    use std::io::BufRead;

    let stdin = std::io::stdin();
    let mut opened: Option<(project::Setup, project::Opened)> = None;
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            return Ok(());
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                println!("{}", failure(&format!("a request must be JSON: {error}")));
                continue;
            }
        };
        // **The command line, rebuilt from the request** — a `Vec<String>` is what `clap` parses, so the request is
        // turned into the arguments a person would have typed.
        let Some(argv) = arguments_of(&request, cli) else {
            println!("{}", failure("the request has no `command` string"));
            continue;
        };
        let parsed =
            match Cli::try_parse_from(std::iter::once("cpp_inspect".to_string()).chain(argv)) {
                Ok(parsed) => parsed,
                Err(error) => {
                    println!("{}", failure(&error.to_string()));
                    continue;
                }
            };
        if matches!(parsed.command, Command::Batch) {
            println!("{}", failure("`batch` cannot ask `batch`"));
            continue;
        }
        if matches!(parsed.command, Command::Caps) {
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "schema": SCHEMA,
                    "ok": true,
                    "command": "caps",
                    "answer": caps::the_capabilities(),
                }))
                .map_err(|error| error.to_string())?
            );
            continue;
        }
        let setup = project::Setup::of(&parsed.project);
        if opened.as_ref().is_none_or(|(known, _)| known != &setup) {
            match project::open(&parsed.project) {
                Ok(fresh) => opened = Some((setup, fresh)),
                Err(error) => {
                    println!("{}", failure(&error));
                    continue;
                }
            }
        }
        let Some((_, session)) = opened.as_mut() else {
            println!("{}", failure("no session"));
            continue;
        };
        match answer_for(session, &parsed.command, &parsed.place) {
            Ok(answer) => {
                let value = answer.value;
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "schema": SCHEMA,
                        "ok": true,
                        "answer": value,
                    }))
                    .map_err(|error| error.to_string())?
                );
            }
            Err(error) => println!("{}", failure(&error)),
        }
    }
    Ok(())
}

/// The argv a request describes, in the vocabulary of the command line.
fn arguments_of(request: &Value, cli: &Cli) -> Option<Vec<String>> {
    let command = request.get("command")?.as_str()?.to_string();
    let mut argv: Vec<String> = Vec::new();

    // The globals first, taking the request's word over the command line's.
    let global = |name: &str, argv: &mut Vec<String>| {
        if let Some(value) = request.get(name) {
            let rendered = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(true) => String::new(),
                Value::Bool(false) => return,
                other => other.to_string(),
            };
            argv.push(format!("--{}", name.replace('_', "-")));
            if !rendered.is_empty() {
                argv.push(rendered);
            }
        }
    };
    for name in [
        "project",
        "file",
        "index",
        "all_files",
        "cook",
        "db",
        "at",
        "needle",
        "occurrence",
        "last",
        "shift",
        "line",
        "col",
        "text",
    ] {
        if request.get(name).is_some() {
            global(name, &mut argv);
            continue;
        }
        // A global the request did not name comes from the command line, when it has one.
        let from_the_command_line = match name {
            "project" => cli
                .project
                .project
                .as_ref()
                .map(|path| path.display().to_string()),
            "file" => cli
                .project
                .file
                .as_ref()
                .map(|path| path.display().to_string()),
            "index" => cli.project.index.then(String::new),
            "all_files" => cli.project.all_files.then(String::new),
            "cook" => cli.project.cook.then(String::new),
            "db" => None,
            "at" => cli.place.at.map(|at| at.to_string()),
            "needle" => cli.place.needle.clone(),
            "occurrence" => Some(cli.place.occurrence.to_string()),
            "last" => cli.place.last.then(String::new),
            "shift" => Some(cli.place.shift.to_string()),
            "line" => cli.place.line.map(|line| line.to_string()),
            "col" => cli.place.col.map(|col| col.to_string()),
            "text" => None,
            _ => None,
        };
        if let Some(value) = from_the_command_line {
            argv.push(format!("--{}", name.replace('_', "-")));
            if !value.is_empty() {
                argv.push(value);
            }
        }
    }
    // …and the fixtures, which are repeatable.
    if let Some(Value::Array(fixtures)) = request.get("fixture") {
        for fixture in fixtures {
            if let Some(text) = fixture.as_str() {
                argv.push("--fixture".to_string());
                argv.push(text.to_string());
            }
        }
    } else if let Some(text) = request.get("fixture").and_then(Value::as_str) {
        argv.push("--fixture".to_string());
        argv.push(text.to_string());
    } else {
        for fixture in &cli.project.fixtures {
            argv.push("--fixture".to_string());
            argv.push(fixture.clone());
        }
    }

    argv.push(command.clone());
    // The command's own arguments.
    match command.as_str() {
        "members" => {
            argv.push("--class".to_string());
            argv.push(request.get("class")?.as_str()?.to_string());
        }
        "grep" => {
            argv.push(request.get("pattern")?.as_str()?.to_string());
        }
        _ => {}
    }
    if let Some(limit) = request.get("limit").and_then(Value::as_u64) {
        argv.push("--limit".to_string());
        argv.push(limit.to_string());
    }
    for name in ["name", "kind", "scope"] {
        if let Some(text) = request.get(name).and_then(Value::as_str) {
            argv.push(format!("--{name}"));
            argv.push(text.to_string());
        }
    }
    Some(argv)
}
