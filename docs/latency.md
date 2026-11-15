# Latency: semantic tokens and completion

The two features an editor asks for *while the reader is typing*, measured over the protocol and inside the server.
Diagnostics has its own document ([`diagnostics-performance.md`](diagnostics-performance.md)); the numbers below are
what these two requests cost, and what dominates them.

## The method

```powershell
# semantic tokens: six requests after a warm open, with the split printed per request
$env:CPPLS_TRACE_SEMANTIC = "1"
python target/bench/lspbench.py target/release/cpp_ls.exe main.cpp --semantic 6 --json out.json

# completion: three shapes, once cold and five rounds warm
python target/bench/completion_latency.py
```

`CPPLS_TRACE_SEMANTIC` prints one line per request: how many names were classified, how many tokens came out, and the
three stages (`classified … | formats … | encoded …`). `target/bench/completion_latency.py` opens a file and asks
completion at three shapes — a member after `.`, a name after `::`, a bare prefix — once cold and five rounds warm.

## The numbers (release, this machine)

**Completion**, realistic TU (35 lines, four standard headers), 153 items at a `.`:

| | latency |
| --- | ---: |
| first ask after `didOpen` | **242 ms** — the cook, not the query |
| every ask after it | **3.0–3.5 ms** |
| a bare prefix (1 item) | 1.1–1.4 ms |

**Semantic tokens**, two sizes:

| file | first request | steady | classified | encoded |
| --- | ---: | ---: | ---: | ---: |
| 35 lines, four headers | 17.9 ms | **0.7–1.0 ms** | — | — |
| 2 701 lines | 203 ms | **29–36 ms** | 29–41 ms | 2 ms |

The 2 701-line file has 7 800 names, of which 2 400 become tokens. **Classification is the whole cost**: 29 ms of
a 36 ms request (≈ 3.7 µs per name), and the encoding — the protocol's delta walk over 2 400 tokens — is 2 ms.

## A project that starts every file with `import std;`

Measured on a real module project (`cpp_modules`: `import std; import math; import report;`, an MSVC compile
database, `std` resolved to MSVC's `modules/std.ixx`) — `target/bench/modules_series.py` and
`modules_latency.py`. The question is what the first seconds cost and what the steady state is.

**Steady state, warm:** completion at `std::` **9–11 ms** for 1000 items (the cap, `isIncomplete` true because of
it); completion at `point.` **0.9–1.3 ms** for 3 items; a diagnostics pull **0.6–0.9 ms** for zero findings. The
`import std;` itself is not a steady-state tax — the 401-file closure is read once and the answers come from the
index.

**The first two seconds are the problem**, and the series shows it plainly:

```text
t+0.0s  std::   88.8 ms,    0 item(s), incomplete=True     ← the module is being read
t+0.2s  std::   23.0 ms,    8 item(s), incomplete=True
t+0.5s  std::    1.1 ms,    8 item(s), incomplete=False    ← eight names, offered as final
t+1.0s  std::    1.1 ms,    8 item(s), incomplete=False
t+2.0s  std::   15.2 ms, 1000 item(s), incomplete=True     ← the real list, capped
```

Two files that both `import std;`, asked in the same seconds (`modules_two_files.py`), show the list *growing* rather
than two different constants:

```text
t+0.5s  main.cpp    8 item(s) | test.cpp  26 item(s)
t+1.0s  main.cpp    8 item(s) | test.cpp  26 item(s)
t+1.5s  main.cpp 1000 item(s) | test.cpp 121 item(s)
```

So the answers are **live queries over an index that is still being filled**: each file reflects however much of the
module's closure had been read and cooked at that moment. Two conclusions follow.

1. **`isIncomplete` must be true while that work exists** — and it now is set when `session.pending_work() > 0`,
   which is measured at t+0.2 s (`incomplete=True` where it used to be `false`). A client that caches a final list
   no longer caches eight names.
2. **`pending_work()` reads 0 at t+0.5–1.0 s while the closure is still landing** — the counters do not see the work
   that fills the index in that window, so the flag is `false` there and the eight-name answer looks final. The
   next step is to find which queue that work is in (the module unit's read is driven by `programs_wanted`, which is
   not part of `pending_work`'s three terms) and count it, rather than to invalidate renderings: a generation-keyed
   rendering cache was implemented and measured for this window and **moved no number at all**, which is what says
   the answers are not stale renderings but partially filled index.

**The first request after `didOpen`** costs 85–235 ms (completion) and 20 ms (pull) — the cook of the file plus the
module's unit, the same cost `diagnostics-performance.md` §4 records for any first request.

## What the numbers say

1. **The warm path is fine.** 3 ms for completion and 0.7 ms for a small file's colours are below what an editor's
   own rendering costs. The 29–36 ms on a 2 700-line file is the one to watch, and Neovim debounces semantic requests
   by **200 ms** by default (`vim.lsp.semantic_tokens`, `debounce = 200`), so it is not on every keystroke — but it
   does occupy the analysis thread when it runs.
2. **The cold request is the cook, in both features.** 242 ms and 203 ms are the same work: the file's rendering is
   built on demand because the request cannot answer from the file's own text. Anything that makes a first request
   cheap has to be about the cook, not about these query paths.
3. **Delta would not help.** Neovim asks for deltas only if the server advertises them, and the delta is computed
   from a freshly classified list — the classification is the cost, not the transfer.
4. **A text-hash cache would help exactly one case**: a refresh with no edit in between (`BufEnter`, a window
   switch, `vim.lsp.semantic_tokens.force_refresh()`). Those are not per-keystroke, but they are not rare either,
   and a cache of the encoded list keyed by the text hash would make them free.

## What is left, in order

1. **Classification cost** — 3.7 µs per name. It is linear, so the wins are per-name: the scope and index questions
   asked for each occurrence. This is the seam where the two features meet, and where the next measurement should go.
2. **A text-hash cache for the encoded tokens**, so a refresh with no edit is free.
3. **The cook**, which the first request of either feature pays — the same item `diagnostics-performance.md` §4
   records, and the largest one to design.
