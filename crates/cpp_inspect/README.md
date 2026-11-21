# `cpp_inspect` — one question about one project, answered as JSON

A binary for asking the analysis what an editor would see, without an editor. It is built **for programs** — its first
client is an AI agent working on this analysis — so the answers are JSON on stdout, the command list is itself a
command, and many questions can be asked in one process.

```text
cpp_inspect --project C:\src\test_cpp --file src/templates.cpp type --needle "Pair<int, int>{1, 2}.sum" --shift 20
cpp_inspect --project C:\src\test_cpp --file src/smart_pointers.cpp hover --needle "node->twin" --text
cpp_inspect --fixture "a.cpp=struct Widget { int size() const { return 1; } };" --needle size hover
cpp_inspect caps
printf '%s\n' '{"command":"hover","needle":"w.size()"}' '{"command":"members","class":"Widget"}' | cpp_inspect --fixture "a.cpp=…" batch
```

## The protocol

One JSON object per answer on stdout:

```json
{"schema":1,"ok":true,"command":"hover","answer":{"at":80,"where":"2:31","markdown":"```cpp\nWidget w\n```…"}}
```

| key | meaning |
| --- | --- |
| `schema` | the protocol's version, `1`. It moves when the envelope's shape does, not when a command is added. |
| `ok` | `true` for an answer, `false` for a failure. |
| `command` | which command answered, so a batch reader can pair lines with requests. |
| `answer` | the command's own value — the same value `--text` renders for a person. |
| `error` | on `ok: false`: `{"code":"failed","message":"…"}`. It is JSON too, so a consumer has one shape to parse. |

`--text` prints the human rendering instead (the format the tool's predecessor used). The exit code says what happened
regardless: `0` answered, `2` failed, `3` the worker panicked.

**`caps`** prints the command list as JSON — name, summary, whether it needs a position, and every argument with its
type and whether it is required. An agent that has never seen this binary starts there.

**`batch`** reads newline-delimited JSON requests on stdin and writes one answer per line. Each request is turned into
the argument list a person would have typed and parsed by the same `clap` grammar, so every flag, default and error
message is the same as the command line's. The session is **reused while the project and file do not change**: opening,
indexing and cooking is the cost, and a series of questions about one file should pay it once.

```json
{"command":"hover","file":"a.cpp","at":80}
{"command":"members","class":"std::shared_ptr<int>","limit":5}
{"command":"diagnostics","file":"a.h","cook":true}
```

## The questions

| command | answers | needs a position |
| --- | --- | --- |
| `type` | the type of the expression at the position | yes |
| `hover` | the hover card the server would show | yes |
| `define` | the declarations of the name, and which one the analysis picks | yes |
| `members` | a class's members, with the declaring class and base depth | — |
| `macro` | the macro in force for the name, and where it was defined | yes |
| `complete` | the completion labels at the position | yes |
| `facts` | a file summary's declarations, filtered | — |
| `inlay` | the inlay hints an editor would draw | — |
| `diagnostics` | the findings the diagnostics channel would report | — |
| `summary` | the summary's key, includes and counts | — |
| `tree` | the syntax tree at the position, innermost first | yes |
| `grep` | the tree nodes whose text contains a pattern | — |
| `inactive` | the regions this configuration does not compile | — |
| `format` | the reading format the index uses | — |
| `stats` | the session's counters | — |
| `caps` | this list | — |
| `batch` | many requests, one answer each | — |

A position is `--at <byte>`, `--needle <text>` (plus `--occurrence`, `--last`, `--shift`), or `--line`/`--col`.

## A project, or a fixture

`--project DIR --file PATH` asks about a real project: the file is read as part of it, and its index is what cross-file
questions see. `--index` indexes every project file (slower, and what questions about the project's *breadth* need);
`--all-files` takes every source file under the root. `--cook` builds the cooked reading — macros expanded, untaken
branches gone — which `diagnostics` needs and a hover on a declaration usually does not.

`--fixture name=text` (or `name=@path`) writes a temporary project, asks the question, and removes it. A fixture is
written **to disk** rather than read from memory on purpose: an in-memory provider would make the fixture's questions go
down a different path from the ones a reader asks in an editor.

## Its own database

The tool indexes into **`inspect.db`** inside the project's cache directory — `<project>/.cppls/inspect.db` by default,
`--db NAME` to change it — and never into the server's `cache.redb`.

That separation is not cosmetic. A summary database carries a `generation` per writer, and a collection on one side
evicts rows the other is using; a tool indexing the same project an editor has open would be that second writer on
every run. Two files, one directory, no interference — and the rows are still shared with the checkout, which is where
they are expensive to rebuild.

## Why a binary and not a test

Every question here used to be a `tests/tmp_*.rs` probe: write it, compile it, read four lines, delete it — and the
compile dominated. That is also why the workspace's probe tests were deleted (they were `#[ignore]`d, and each one cost
a link): a question is an argument now, and the next question does not need a new crate target.

The two remaining probes are the C-project measurements the C-usability work is driven by
(`cpp_ls/tests/c_diagnostics_probe.rs` and `cpp_code_analysis/tests/c_name_lookup_probe.rs`); everything this tool can
answer is answered by this tool.
