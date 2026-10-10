# The migration: one semantic authority

## The measurement that settles it

C++ source with macros unexpanded is not a program, so a raw reading cannot be a semantic authority. Measured on
MSVC's `<xutility>` with `examples/cooked_parse.rs`:

```text
raw reading  (  244 021 bytes):   84 error node(s)
    29x "}"     12x "requires"    12x "{"     6x "return"     6x "} // clang-format on"
cooked reading (1 095 871 bytes, 77 file(s) stitched):    0 error node(s)
```

The unit reading — every file the walk reached, stitched in include order, all 77 of them, branches taken — parses
**clean**. Every parse defect chased over the previous rounds (`_RANGES iter_move` in a compound requirement,
`basic_string` missing from `<xstring>`, `basic_ios` at file scope, the `_THROW` assertion) is an artefact of
reading text no compiler ever sees.

## The defect, stated exactly

The index holds **both** readings and makes the **raw** one primary
(`index/project.rs:6290`, `visible_declarations_upto`):

```rust
match group {
    None => raw.extend(summary.declarations.iter().filter(…)),
    Some(group) => raw.extend(group.iter().filter(|p| !p.is_cooked())…),
}
for fact in &raw { found.push(VisibleDeclaration { … }) }

let Some(cooked) = cooked else { continue };

// …and what the file was **cooked** into, **minus what the raw reading already said**.
// `(name, kind)` is the identity a candidate is deduplicated by.
```

So a reading with 84 parse errors is the default answer, and the reading with 0 errors may only **add** what the
broken one did not already claim. Where the two disagree about the same name, the broken one wins by construction.

That single precedence rule explains every symptom this audit chased:

```text
basic_ios at file scope AND in std     both facts exist; the raw one is found first
basic_string absent from the index     the raw reading cannot parse its template head, and the
                                       cooked fact that could add it is exactly what is subtracted
std::ios:: offering nothing            the base clause came from the raw fact
"why did adding cooked facts make     it did not poison anything: it was measured against a
 the readings worse?"                  dedup that had already decided which reading wins
```

## The target

```text
                    today                        target
semantic answer     raw, with cooked added      cooked, raw only where cooked has nothing
facts from          both readings                the unit reading (whole program, one parse)
raw reading serves  semantics, positions         positions and lexing ONLY
```

The rule to implement, in one sentence: **semantic answers come only from the cooked reading; the raw reading
serves positions, highlighting and macro editing, and makes no claim about what a name means.**

The reason the raw reading cannot simply be deleted is that it is the only reading most files have today: the unit
reading is implemented ([`Session::read_the_unit`], `FileIndexer::index_unit_rendering`) but **not wired into the
pump**, because doing so was measured as a regression and that regression was never explained:

```text
                                  without the unit read   with it
declarations_in("std") after draining          2008        1959
definition, resolved in a header                 50          46
definition, the index has no such name            8          12
```

**The precedence rule above is the explanation, and it was in the code the whole time.** The unit reading is the
only path that gives *every* file a cooked reading; while raw wins, adding it can only reshuffle a dedup whose
winner is already chosen — which is why more correct information produced worse answers, a result the existing note
calls "not understood yet".

## The order of work

Each step is independently checkable, and the first two are measurements rather than changes.

```text
1.  Explain the unit-read regression with the precedence rule as the hypothesis.
    Wire it in, and check the three readings above. If the hypothesis is right they improve;
    if they do not, the hypothesis is wrong and nothing else should be built on it.
    Instrument: std_probe --cooked-index (prints the two readings' declaration counts per file).

2.  Reverse the precedence in `visible_declarations_upto`: cooked first, raw only for a
    file that has no cooked reading. The dedup identity is already `(name, kind)`; it
    becomes "a cooked fact suppresses the raw fact of the same identity" rather than the
    reverse.

3.  Stop producing raw facts for semantics. `index/mod.rs:666` builds facts from the raw
    tree; once every file in view has a cooked reading (step 1) there is no gap for it to
    fill. Keep `FileView::parse` for positions, highlighting and macro editing only.

4.  Then the fallbacks. `session.rs` returns the raw reading for a *semantic* answer in
    three places when no rendering is cached (2378, 2417, 2502). With step 3 these become
    "defer the answer" rather than "answer from the wrong text".

5.  Then the patches that exist only because of the split, which should be deleted rather
    than kept: `bases_of`'s bare-name second query (a workaround for `basic_ios` having two
    scopes), and the raw/cooked asymmetry noted throughout `docs/`.
```

## What would falsify the direction

Stated so that it can be checked rather than believed:

* **The cooked reading is expensive.** Cooking one file is measured at 9–269 ms, and `read_the_modules` costs
  6151 ms cold. If step 1 shows the unit reading cannot be afforded on the pump, the target becomes "cooked where
  available, defer otherwise" rather than "cooked always", and step 3 waits.
* **Some file has no cooked reading at all.** A file not reached by any rendered unit — a file whose closure is
  unknown — would have nothing. Step 2's rule keeps raw as the answer for exactly that case, so this is handled by
  construction rather than by an exception.
* **The unit reading's facts are worse for some file.** Possible, and step 1's instrument names the file. That
  would be a defect **in the unit reading** to fix, not a reason to prefer the reading with 84 parse errors.
