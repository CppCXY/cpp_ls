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
