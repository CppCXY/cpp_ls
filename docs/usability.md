# Getting to "usable": what is fixed, what is measured, what is left

## The request gates: two of three now answer instead of refusing

A request that takes the **whole project's** queue as its condition refuses for the entire first load. Three handlers did
that, and each is a different kind of request, so they do not deserve the same answer:

```text
references        a READ: a list of uses. Answered now from what is indexed, and the count of still-queued files is
                  logged at `info` so a thin answer is explainable. A partial list that grows is what clangd answers.
workspace_symbol  a READ: a search. Same change, same reason.
rename            a WRITE into the user's files. The gate stays: a partial search would rename some uses and leave
                  others, which is a broken build written to disk. What it still needs is a **reason** — an LSP error
                  or a `window/showMessage` saying how many files are queued, instead of a silent `null` that an editor
                  shows as "no occurrences".
```

Both read gates have a test that builds the queued state deterministically rather than waiting for it, and both were
**rewritten** rather than deleted: `a_reference_list_is_partial_while_the_index_has_work_and_complete_after` and
`a_symbol_search_is_answered_while_the_index_has_work_and_complete_after`. Each asserts the partial answer **and** that
the same call is complete after `index_everything`, so a fix that answered nothing would still fail.

## The latency test is flaky now, and that is progress

```text
last round's full suite   54 ok / 0 failed
this round's full suite   53 ok / 1 failed  — the same latency test
```

It is the same test and the same code: the outlier documented in `docs/latency.md` (one request in about twelve loses
~1.7 s to **executor starvation** — the pump's CPU phases run inline on the async runtime, and the session's own budget
warning never fires) moves between requests and decides whether it lands inside the steady-state window. Before this
work the test failed **every** run on the first completion (3309 ms against 2500); now it fails only when the outlier
lands late. The next measurement is in that document: put the pump's read-lock phase behind `spawn_blocking` and see
whether the outlier disappears.

## Still to do, in the order the goal set

```text
2b  rename's refusal needs a reason (see above) — the gate itself is right
3   the 130 s cold index decomposed into stages, with a probe, before any further performance work
```
## The refused rename now says why — and why it is the only one that still refuses

`RequestOutcome` has `Ready`, `Cancelled` and `Missing`, and **no error variant**, so a handler cannot answer an LSP
error without a protocol-layer change. What it can do is send `window/showMessage`, and `prepareRename` — the call a
client makes *before* it shows the rename box — now sends one when the index still has work:

```text
cpp_ls is still reading the project (N file(s) left), so a rename would miss uses. The editor offers it again when the
index has caught up.
```

Sent from `prepareRename` only, not from `rename` too: a client calls both in that order, and two messages per attempt is
worse than one. Measured by compiling and reading the file — there is **no test** for it, because a handler test would
have to observe a notification the harness does not read, and that is recorded rather than pretended.

### Where the three gates ended up

```text
references        answers from what is indexed; the count of queued files is logged at info
workspace_symbol  the same
rename            still refuses while anything is queued — a partial rename writes a broken build — and now says why
```

### The two handoffs this goal could not close, both measured and both in `docs/cold-start-cpu.md` / `docs/latency.md`

```text
executor starvation    one completion in about twelve loses ~1.7 s with **no** session hold: the pump's CPU phases run
                       inline on the async runtime. The next measurement is `spawn_blocking` around them.
template-class members `nlohmann::json::push_back` is reported missing because the member list the index holds for a
                       template class is partial. That is not a guard; it is the member read.
```
