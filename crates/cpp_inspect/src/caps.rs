//! **What this tool can answer** — the machine-readable half of the protocol.
//!
//! An agent that has never seen this binary has to be able to find out what it does without reading the source, so
//! `cpp_inspect caps` prints this list: every command, what it answers, which arguments it takes and whether it needs a
//! position. The strings here are the **single source of truth** for that answer — a command that is added to
//! [`crate::Command`] and not here is a command an agent cannot discover, so the two lists are checked against each
//! other by `main`'s `--help` text and by the README's table.
//!
//! The shape is deliberately flat: `{"commands": [{"name": …, "summary": …, "position": bool, "args": [{…}]}]}`. A
//! schema-description language would be more expressive and would also be a second thing to keep in step; the questions
//! are fifteen and the arguments are a handful of strings and numbers.

use serde_json::{Value, json};

/// One argument a command takes.
struct Argument {
    name: &'static str,
    /// `string`, `number`, `bool` or `enum`.
    kind: &'static str,
    /// What it is for, in one sentence.
    summary: &'static str,
    /// `true` when the command cannot be asked without it.
    required: bool,
}

const fn argument(
    name: &'static str,
    kind: &'static str,
    summary: &'static str,
    required: bool,
) -> Argument {
    Argument {
        name,
        kind,
        summary,
        required,
    }
}

/// One command.
struct Command {
    name: &'static str,
    summary: &'static str,
    /// Does it need `--at`/`--needle`/`--line`?
    position: bool,
    args: &'static [Argument],
}

const fn command(
    name: &'static str,
    summary: &'static str,
    position: bool,
    args: &'static [Argument],
) -> Command {
    Command {
        name,
        summary,
        position,
        args,
    }
}

const COMMANDS: &[Command] = &[
    command(
        "type",
        "The type of the expression at the position, as the analysis spells it.",
        true,
        &[],
    ),
    command(
        "hover",
        "The hover card the server would show at the position, as markdown.",
        true,
        &[],
    ),
    command(
        "define",
        "The declarations of the name at the position: the one the analysis picks, and every other one it knows.",
        true,
        &[],
    ),
    command(
        "members",
        "The members of a class, with the class that declares each and how many base steps away.",
        false,
        &[
            argument(
                "class",
                "string",
                "The class, as an editor would write it: `std::shared_ptr<int>` or `Widget`.",
                true,
            ),
            argument(
                "limit",
                "number",
                "How many to print; 0 is all of them. Default 0.",
                false,
            ),
        ],
    ),
    command(
        "macro",
        "The macro in force for the name at the position, with the file and line it was defined at.",
        true,
        &[],
    ),
    command(
        "complete",
        "The completion labels the server would offer at the position.",
        true,
        &[argument(
            "limit",
            "number",
            "How many to print. Default 40.",
            false,
        )],
    ),
    command(
        "facts",
        "The declarations a file's summary holds, filtered by name, kind or scope.",
        false,
        &[
            argument(
                "name",
                "string",
                "Only facts whose name contains this.",
                false,
            ),
            argument(
                "kind",
                "enum",
                "One of `type`, `function`, `variable`, `namespace`, `macro-like`, `other`.",
                false,
            ),
            argument(
                "scope",
                "string",
                "Only facts whose scope contains this.",
                false,
            ),
            argument("limit", "number", "How many to print. Default 40.", false),
        ],
    ),
    command(
        "inlay",
        "The inlay hints an editor would draw for the file: parameter names and deduced types.",
        false,
        &[],
    ),
    command(
        "diagnostics",
        "The findings the diagnostics channel would report for the file, as `code`, `range` and `message`.",
        false,
        &[],
    ),
    command(
        "summary",
        "The file summary's key, its includes and its counts.",
        false,
        &[],
    ),
    command(
        "tree",
        "The syntax tree at the position, innermost node first — kind and text, with the ancestors.",
        true,
        &[argument(
            "limit",
            "number",
            "How many ancestors to print. Default 20.",
            false,
        )],
    ),
    command(
        "grep",
        "The syntax-tree nodes whose text contains a pattern — the whole tree, not a position.",
        false,
        &[
            argument(
                "pattern",
                "string",
                "What the node's text must contain.",
                true,
            ),
            argument(
                "limit",
                "number",
                "How many nodes to print. Default 40.",
                false,
            ),
        ],
    ),
    command(
        "inactive",
        "The regions of the file this configuration does not compile — what an editor greys out.",
        false,
        &[],
    ),
    command(
        "format",
        "The reading format the index on disk uses.",
        false,
        &[],
    ),
    command(
        "stats",
        "The session's own counters after the setup: files, index, cache.",
        false,
        &[],
    ),
    command(
        "caps",
        "This list. The one command that needs no project and no file.",
        false,
        &[],
    ),
    command(
        "batch",
        "Read newline-delimited JSON requests on stdin and answer each on one line of stdout.",
        false,
        &[],
    ),
];

/// The list, as the protocol prints it.
pub fn the_capabilities() -> Value {
    let commands: Vec<Value> = COMMANDS
        .iter()
        .map(|command| {
            json!({
                "name": command.name,
                "summary": command.summary,
                "position": command.position,
                "args": command
                    .args
                    .iter()
                    .map(|argument| json!({
                        "name": argument.name,
                        "type": argument.kind,
                        "summary": argument.summary,
                        "required": argument.required,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "schema": crate::SCHEMA,
        "protocol": "JSON on stdout. One object per answer: `schema`, `ok`, `command`, `query`, `answer`; on a \
                     failure `ok` is false and `error` carries `code` and `message`. `--text` prints the \
                     human rendering instead.",
        "commands": commands,
        "globals": [
            {"name": "project", "type": "path", "summary": "The project root. The index is what cross-file questions see."},
            {"name": "file", "type": "path", "summary": "The file the question is about, absolute or relative to `--project`."},
            {"name": "fixture", "type": "string", "summary": "`name=text` or `name=@path`, repeatable: a temporary project the tool writes and removes."},
            {"name": "index", "type": "bool", "summary": "Index every project file instead of only the one asked about."},
            {"name": "all_files", "type": "bool", "summary": "With `--index`, take every source file under the project."},
            {"name": "cook", "type": "bool", "summary": "Build the cooked reading — macros expanded, untaken branches gone. `diagnostics` needs it."},
            {"name": "db", "type": "string", "summary": "The database file inside the project's cache directory. Default `inspect.db`."},
            {"name": "at", "type": "number", "summary": "A byte offset in the file's own text."},
            {"name": "needle", "type": "string", "summary": "Text to find; the offset is its first byte, adjusted by `occurrence`, `last` and `shift`."},
            {"name": "occurrence", "type": "number", "summary": "Which occurrence of `--needle`, 1-based. Default 1."},
            {"name": "last", "type": "bool", "summary": "The last occurrence of `--needle`."},
            {"name": "shift", "type": "number", "summary": "Bytes from the resolved position."},
            {"name": "line", "type": "number", "summary": "A 1-based line."},
            {"name": "col", "type": "number", "summary": "A 1-based byte column, with `line`."},
            {"name": "json", "type": "bool", "summary": "JSON on stdout. The default."},
            {"name": "text", "type": "bool", "summary": "The human rendering instead of JSON."},
        ],
    })
}
