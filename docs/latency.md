# Latency: where a request waits
## The insert wave is sized by what it costs, measured

The long-red latency test's first completion was **3309 ms** against a 2500 ms budget, and the pump's own log names the
mechanism: a wave of `INDEX_SLICE = 32` files is committed under **one** write hold, and a completion that arrives inside
that window waits for all of it. Measured by experiment, on the same machine and the same fixture:

```text
INDEX_SLICE = 32    completion #0 3309 ms   #4 1528 ms   steady (after #5) 240-364 ms
INDEX_SLICE = 4     completion #0  137 ms   #4   12 ms
```

A fixed 4 pays the per-wave overhead forever, so the pump now **searches** for the size: the wave it asks for starts at
`THE_FIRST_WAVE` (8), **halves** when the write lock was held over `INDEX_SLICE_MS` (40), and **grows by one** when it
was held for under half of that. `INDEX_SLICE` stays as the cap.

Measured with the search in place:

```text
completion #0 176 ms  #1 4  #2 5  #3 58  #4 15  #5 8  #6 27  #7 **1735**  #8 35  #9 37  #10 55  #11 72
```

So: the first wave is fixed (3309 → 176 ms, and #4's second long wait 1528 → 15), the steady state improved from
240-364 ms to 35-72 ms — and the test **still fails**, on two things this change did not touch:

```text
#7 = 1735 ms     one single write hold, late in the fill — the cooking insert or the re-read drain, not the wave
#10/#11 = 55/72  the steady-state budget (50 ms), which every attempt before this change also missed (240-364)
```

That is the next measurement: the second `update_session("index slice")` already times its two halves (`drained`,
`inserted`) — the number is in the log line it writes, and the wave search is done. Nothing else was changed, and the
suite is unchanged: 53 targets ok, the latency test the only red.
## The second measurement: the write locks are milliseconds, and one request still loses 1.7 s

The pump already logs its own split, and the latency harness writes its log **into the project directory** (`<root>/log`,
level `debug`) — not the shared `%LOCALAPPDATA%` one, which is why the first attempt found nothing:

```text
index slice: prepared 32, committed 8, write lock held **1 ms**, 1 to cook, 22 left
index slice: prepared 0,  committed 4, write lock held **0 ms**, …
```

So after the wave search the write holds are **0–3 ms**. Three candidate causes were then measured and **two were
refuted**:

```text
RE_READ_SLICE 4 -> 1    #7 1735 -> 1629 ms   not the re-read slice
COOK_A_SLICE  4 -> 1    #7 1735 -> 1697 ms   not the cooking slice
```

Both experiments were reverted, because a change that does not move the number is not a change.

And the session's own budget warning — which names the label that held the analysis — fires **once** in the whole run:

```text
`open the session` held the analysis for 66 ms, over the 50 ms budget
```

There is **no** 1.7 s hold on the session. So the slow request is not waiting for a lock: it is waiting for the
**executor**. The pump runs CPU-heavy phases (a translation unit per re-read file, a parse per wave file) **inline on the
async runtime**, so one request in about twelve loses a scheduling gap of seconds while every other one answers in
30–70 ms — which is exactly the shape the numbers show (one outlier, the rest fast, varying which request is the
outlier).

That is the next measurement, and it is one line of code to test: put the pump's read-lock phase behind
`tokio::task::spawn_blocking` and see whether the outlier disappears. Nothing else was changed, the suite is unchanged
(53 targets ok, the latency test the only red), and the wave search is the one improvement kept from this round: the
**first** completion went from 3309 ms to ~175 ms.
## The starvation fixed: the pump's CPU phases moved off the runtime

The measurement said the slow request waited for no lock: the session's budget warning never fired, and one completion in
about twelve lost ~1.7 s anyway. That is the executor, not the analysis — the pump ran `prepare_a_wave`,
`prepare_the_drain` and `render_them` **inline in an `async` task**, so a wave of parses took the runtime thread away from
every request in flight for its whole length.

The fix is the one the queries already use: `AnalysisState::run_blocking`, which takes a blocking permit, moves the
closure to `spawn_blocking`, and holds the read guard on **that** thread.

Measured on the latency fixture, three runs of the same test:

```text
before   … #7 1735 ms, #11 1697 ms, #9 1629 ms   (an outlier in every run, moving between requests)
after    run 1   490  5  7  41  16  16  26  7  147  52  39  54
         run 2   273  6 30  33  16   7  25 11  154  52  45  37
         run 3   232  7 37  32  20  19  24 17  142  49  44  42
```

The outlier is gone: the worst completion is now ~490 ms, and that is the **first** one, before the index has anything to
answer from.

**What the test still says** is a different, smaller thing: the steady-state budget is 50 ms and the eighth completion is
~145–155 ms in every run. That is a spike of ~100 ms, not a starvation, and it lands on the same request index every time
— so it is a phase of the pump that the fix did not move: the write-lock commit, or the cooking claim between it. That is
the next measurement, and it is a much smaller one than the 1.7 s was.
