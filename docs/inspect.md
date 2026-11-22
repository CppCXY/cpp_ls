# `cpp_inspect` — the protocol

One binary that answers one question about one project, for **programs** rather than people. Its first client is an AI
agent working on this analysis, so the answers are JSON on stdout, the command list is itself a command, and many
questions can be asked in one process.

```text
cpp_inspect --project C:\src\test_cpp --file src/templates.cpp type --needle "Pair<int, int>{1, 2}.sum" --shift 20
cpp_inspect --project C:\src\test_cpp --file src/smart_pointers.cpp hover --needle "node->twin" --text
cpp_inspect --fixture "a.cpp=struct Widget { int size() const { return 1; } };" --needle size hover
cpp_inspect caps
```

The implementation is `crates/cpp_inspect`; its `README.md` is a short version of this file.

## 1. The envelope

One JSON object per answer on stdout:

```json
{"schema":1,"ok":true,"command":"hover","answer":{"at":80,"where":"2:31","markdown":"```cpp\nWidget w\n```…"}}
```

| key | meaning |
| --- | --- |
| `schema` | the protocol's version — `1`. It moves when the **envelope's shape** does, not when a command is added. |
| `ok` | `true` for an answer, `false` for a failure. |
| `command` | which command answered, so a batch reader can pair each line with its request. |
| `answer` | the command's own value — the same value `--text` renders for a person. A command's `answer` is described in §3. |
| `error` | on `ok: false`: `{"code":"failed","message":"…"}`. JSON as well, so a consumer has one shape to parse. |

The exit code says the same to a shell: `0` answered, `2` failed, `3` the worker panicked. `--text` prints the human
rendering instead of the envelope; it is never mixed with JSON, so a pipeline can rely on stdout being one or the other.

**No progress, logs or warnings ever go to stdout.** The debug-build warning and `--time` go to stderr, so a consumer
that reads stdout line by line is never surprised by a line that is not an envelope.

## 2. `caps` — the list an agent starts from

`cpp_inspect caps` prints every command as JSON: name, one-sentence summary, whether it needs a position, and each
argument with its type and whether it is required. It is the **only** place the vocabulary is written down; a command
added to the binary and not to `caps` is a command an agent cannot find.

```json
{"schema":1,"ok":true,"command":"caps","answer":{
  "protocol":"JSON on stdout. …",
  "commands":[{"name":"type","summary":"The type of the expression at the position, as the analysis spells it.",
               "position":true,"args":[]}, …],
  "globals":[{"name":"project","type":"path","summary":"The project root. …"}, …]}}
```

## 3. The questions

| command | `answer` | needs a position |
| --- | --- | --- |
| `type` | `{at, where, type}` or `{at, where, unknown}` | yes |
| `hover` | `{at, where, markdown}` | yes |
| `define` | `{at, where, chosen, declarations:[…]}` | yes |
| `members` | `{class, count, members:[{name, kind, declared_in, depth, …}]}` | — (`--class`) |
| `macro` | `{at, where, name, defined_at, body}` | yes |
| `complete` | `{at, where, items:[…]}` | yes |
| `facts` | `{file, matched, total, facts:[{name, kind, line, column, scope, type_of, returns, …}]}` | — (filters) |
| `inlay` | `{file, hints:[{line, column, label, kind}]}` | — |
| `diagnostics` | `{file, reading, unplaced, findings:[{code, line, column, message}]}` | — |
| `summary` | `{file, key, includes:[…], counts:{…}}` | — |
| `tree` | `{at, nodes:[{kind, text, …}]}` (innermost first) | yes |
| `grep` | `{pattern, matched, nodes:[{kind, text, …}]}` | — (`pattern`) |
| `inactive` | `{file, regions:[{from, to, line, column}]}` | — |
| `format` | `{format_version, reading_fingerprint}` | — |
| `stats` | `{text}` — the session's counters | — |
| `sweep` | `{files_answered, findings, codes:[{code,count}], by_file:[{file,findings:[…]}]}` | — (`--code`, `--including`) |
| `caps` | the command list | — |
| `batch` | one envelope per input line | — |

A position is `--at <byte>`, `--needle <text>` (plus `--occurrence`, `--last`, `--shift`), or `--line`/`--col`. Every
`answer` that used a position carries `at` (the byte) and `where` (`line:column`), so a consumer can echo back what was
asked without recomputing it.

## 4. `batch` — many questions, one process

Newline-delimited JSON requests on stdin, one envelope per line on stdout:

```json
{"command":"hover","file":"a.cpp","at":80}
{"command":"members","class":"std::shared_ptr<int>","limit":5}
{"command":"diagnostics","file":"a.h","cook":true}
```

Each request is turned into **the argument list a person would have typed** and parsed by the same `clap` grammar the
command line uses, so every flag, default and error message is the command line's — a second parser would be a second
protocol to keep in step.

The session is **reused while the project and file do not change**. Opening, indexing and cooking a project is the cost,
and a series of questions about one file should pay it once. A line that fails to parse, or whose request has no
`command`, gets an `ok:false` envelope and the run continues.

## 5. A project, or a fixture

* `--project DIR --file PATH` — a real project: the file is read as part of it, and its index is what cross-file
  questions see.
* `--index` — index every project file (slower; what questions about the project's *breadth* need).
* `--all-files` — with `--index`, take every source file under the root.
* `--cook` — build the cooked reading (macros expanded, untaken branches gone). `diagnostics` needs it; a hover on a
  declaration usually does not.
* `--fixture name=text` or `name=@path` — a temporary project the tool writes, asks about, and removes. `--keep` leaves
  it and prints its root on stderr.
* `--time` — the setup's timings, on stderr.

A fixture is written **to disk** rather than read from memory on purpose: an in-memory provider would make the
fixture's questions go down a different path from the ones a reader asks in an editor.

## 6. Its own database

The tool indexes into **`inspect.db`** in the project's cache directory — `<project>/.cppls/inspect.db`, `--db NAME` to
change it — and never into the server's `cache.redb`.

That separation is not cosmetic: a summary database carries a `generation` per writer, and a collection on one side
evicts rows the other is using. A tool indexing the same project an editor has open would be that second writer on every
run. Two files, one directory, no interference — and the rows are still shared with the checkout, which is where they
are expensive to rebuild.

## 6.5 `sweep` — a whole project's findings

The question the deleted probe tests answered by hand. Every file the session holds is cooked and diagnosed, and the
findings are grouped by code with a per-file list under them:

```text
cpp_inspect --project E:\corpora\redis --file src/dict.c sweep
cpp_inspect --project E:\corpora\redis --file src/dict.c sweep --code unresolved-call-or-receiver --including "redis\src"
```

`--index --all-files` are **implied** — a sweep of one file is what `diagnostics` already is. `--code` keeps just the
named codes and `--including` keeps just the files whose path contains a substring; the two filters every measurement in
`docs/worklog.md` needs, because a project measured against a platform it was not written for has classes that are the
platform's fault and must be named as such rather than counted as defects.

The answer is the same `Session::diagnostics` the server answers a pull with, so a number here and a squiggle there are
the same claim; what the sweep adds is that a **project** is measured rather than a file. On redis 8.x it answers 222
files and 1 429 findings in **74 s** — the same work the probe did in 77 s, without a second client.

## 7. Why this exists



Every question here used to be a `tests/tmp_*.rs` probe: write it, compile it, read four lines, delete it — and the
compile dominated. The workspace's `#[ignore]`d probe tests were deleted for the same reason (each cost a link in every
gate); see `docs/worklog.md` and `c-support.md` §8.4's record. A question is an argument now.

**Nothing else measures the analysis now.** The two probe tests the worklog's earlier numbers came from —
`cpp_ls/tests/c_diagnostics_probe.rs` (a whole project's findings, over the LSP protocol) and
`cpp_code_analysis/tests/c_name_lookup_probe.rs` (one name's visibility) — were deleted once `sweep`, `facts` and
`define` answered the same questions here. Every crate now has exactly **one** test target, which is what a gate links.
