# `cpp_inspect` — one question about one project, answered as JSON

A binary for asking the analysis what an editor would see, without an editor. It is built **for programs** — its first
client is an AI agent working on this analysis — so the answers are JSON on stdout, the command list is itself a
command, and many questions can be asked in one process.

```text
cpp_inspect --project C:\src\test_cpp --file src/templates.cpp type --needle "Pair<int, int>{1, 2}.sum" --shift 20
cpp_inspect --fixture "a.cpp=struct Widget { int size() const { return 1; } };" --needle size hover
cpp_inspect caps
printf '%s\n' '{"command":"hover","needle":"w.size()"}' '{"command":"members","class":"Widget"}' | cpp_inspect --fixture "a.cpp=…" batch
```

The answer is one JSON object: `{"schema":1,"ok":true,"command":"hover","answer":{…}}`, and a failure is the same shape
with `ok:false` and an `error`. `--text` prints the human rendering instead. `caps` lists every command as JSON, and
`batch` reads newline-delimited requests on stdin, reusing one session while the project and file stay the same.

The tool writes its own database — `inspect.db` in the project's cache directory — so it is safe beside a running
editor: two writers over one database would share a `generation`, and a collection on one side evicts rows the other is
using.

**The protocol, the command list and what each answer carries: [`docs/inspect.md`](../../docs/inspect.md).** What the
tool replaced, and why: [`docs/worklog.md`](../../docs/worklog.md).
