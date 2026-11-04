# `inspect` — one question about one project

```text
cargo run -q --release -p inspect -- <command> [where] [how much to read]
```

The analysis answers about twenty questions over and over — the type at an offset, a class's members, the hover card, the
fact behind a name, the macro in force. Every one of them used to be a new `tests/tmp_*.rs` probe: write it, compile it,
read four lines, delete it. The compile dominated, the probe list grew, and the answers were the same twenty questions.

This is one binary that answers all of them from arguments. `cargo run --release` is the intended way: a debug build
answers the same questions, minutes later (the tool says so on stderr).

## Where the question is

A question is about a position, and a position is one of:

| flag | meaning |
|---|---|
| `--at <byte>` | a byte offset in the file's own text |
| `--needle <text>` | text to find; `--occurrence <n>` (1-based), `--last`, and `--shift <bytes>` adjust it |
| `--line <n> --col <n>` | a 1-based line and byte column |

`--shift` is what a report usually needs: `--needle "node->twin" --shift 6` is the `t` of `twin`.

## Which project, which file

| flag | meaning |
|---|---|
| `--project <dir> --file <path>` | a project on disk; the file is opened, caught up and viewed |
| `--fixture name=text` (repeatable) | a temporary project the tool writes and removes; `name=@path` reads the text from disk |
| `--index` | index every project file (`--all-files` makes that the whole tree under `--project`) |
| `--cook` | build the file's cooked reading — its whole include closure, macros expanded |
| `--time` | report the setup's cost per step on stderr |

## The commands

| command | the question it answers |
|---|---|
| `type` | what type the expression at the position has (`Session::type_at`) |
| `hover` | the card the server would show (`cpp_ls`'s own rendering, not a second one) |
| `define` | the declaration the analysis picks for the name, and every one it knows |
| `members --class <name>` | a class's members, with the class that declares each and how many bases away |
| `macro` | the `#define`/`#undef` in force for the name under the position |
| `complete` | the labels offered at the position, with their details |
| `facts [--name] [--kind] [--scope]` | the facts a file's summary holds |
| `diagnostics` | the findings a diagnostics channel would report |
| `summary` | the summary's key hashes, includes and counts |
| `tree` | the syntax tree at the position, innermost node first |
| `format` | the reading format the index on disk uses |
| `stats` | the session's counters after the setup |

Add `--json` (global) for the same answer as a machine-readable value.

## The cost, and why the defaults are lazy

Measured on `test_cpp` (16 files; its closure reaches the standard library, ~1 700 files):

```text
type, default                      90 ms
type --cook --index, cold cache     212 s
members --cook --index, cold cache  347 s
members --cook --index, warm cache  see below
```

So the defaults do **not** cook and do **not** index everything: the session prepares what a query needs when the query
asks (`members_of` reads the header that declares the class, `type_at` reads the files its inference reaches). That is
the architecture's own rule, and it is what makes a question cost 90 ms instead of a server's cold start.

`--cook` and `--index` are the two steps that read the file's **whole include closure**; on a cold summary cache that is
minutes, and it is the same work the server pays at startup (parallel, ~18 s). They are for the questions that genuinely
need the compiler's view (`diagnostics` needs the cooked reading) or the project's breadth.

## Recipes

The questions this session actually asked, as commands:

```powershell
# The type of a member of a class-template instantiation (report: "`int` has no member `sum`").
inspect type --project $p --file src/templates.cpp --needle "Pair<int, int>{1, 2}.sum" --shift 20

# The card for a member reached through a smart pointer (report: "shared_ptr has no member twin").
inspect hover --project $p --file src/smart_pointers.cpp --needle "node->twin" --shift 6

# A class's members, including inherited ones and the class that declares each.
inspect members --project $p --file src/smart_pointers.cpp --class "std::shared_ptr<int>"

# The facts behind a widget: what the summary thinks `call` is, with its qualifiers and requires-clause.
inspect facts --project $p --file src/strings.cpp --name call

# What a macro use jumps to (report: "find definition on a #define goes to the wrong place").
inspect macro --project $p --file src/preprocessor.cpp --needle "TEST_CPP_SQUARE" --last

# A self-contained question, no project on disk.
inspect hover --fixture "a.cpp=struct Widget { int size() const; };\nvoid f() { Widget w; w.size(); }" --needle "size()" --last

# The same answers for a script.
inspect facts --project $p --file src/strings.cpp --name call --json
```
