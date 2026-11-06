# What to do next

Written after reading the commits between `09939af` (the hover rendering) and `305a927` (the decorated constructor
fix), `docs/buglist.md` §27–§30, `docs/refinement.md` and the two smart-pointer reports. Ordered by what unblocks what,
with the measurement that would settle each item.

---

## 1. The smart pointer's two halves — **fixed for the dot**, one mechanism left

Measured after the fix, on `test_cpp/src/smart_pointers.cpp` (`inspect complete --index`):

```text
shared.        scope `std::shared_ptr`, 12 items   operator=, shared_ptr, element_type, weak_type, get,
                                                   operator bool, owner_before, reset, swap, unique, use_count, ~
copied->       scope `Resource`, 2 items           identifier, ~Resource
hover `reset`  four overloads rendered              void reset() / _SafeConv<_Yp> reset(_Yp* __p) / …
```

The dot used to follow the pointer protocol (`element_type`), so `shared.` listed the *pointee's* two members and none
of the handle's — the report "`shared_ptr`'s own fields are not completed". Only an `arrow` follows the protocol now;
a raw pointer left of a dot still follows its pointee and still reports `followed_a_pointer` for the client's rewrite.
`completion::a_dot_on_a_smart_pointer_offers_its_own_members` is the gate. What is still missing from the twelve:
`operator*`/`operator->`, which libstdc++ declares on `__shared_ptr_access` — the base walk does not reach it, and
`members_of("std::shared_ptr")` reports no unlisted base. That is the item below.

### What is left of this section

The reports: **`shared_ptr`'s own fields are not completed**, and **`reset()` is not recognised**. The last two commits
(`3820e24`, `3bc61f6`) fixed the *arrow* (`state_->` reaches `State`'s members, which needed the standard's pointer
protocol: a class that declares `element_type` is pointer-like, and libstdc++ keeps `operator->` two bases down). What
they did **not** touch is the handle itself.

Measured on `test_cpp/src/smart_pointers.cpp` with the binary before those commits (so this is a description of the
shape, not the current answer — item 1.0 is to re-run it):

```text
complete after `shared.`     12 items, `reset`/`get`/`use_count` among them (inherited from `std::__shared_ptr<_Tp>`)
hover on `reset`             "`shared_ptr<Resource>` has no member `reset`"
members_of("shared_ptr<Resource>")   42 members, `reset` ×4 at depth 1
members_of("std::shared_ptr")        15 members — own members only, no base walk
```

Two mechanisms explain the disagreement, and both are in the code:

1. **`members_of` retries the qualified spelling only when the list is empty** (`Known::Yes(list) if
   list.members.is_empty()`). A **partial** list — the 15 own members, because the base's file was not read — is
   returned as the answer, and the hover that looks for `reset` in it says "has no member".
2. **The hover's member loop keeps the first list it is given** (`list = list.or(Some(candidates))`) and only breaks
   when the name is found. For a dot access there is one class, so a partial list is the answer; for an arrow there are
   two (the pointee, then the written type) and the *pointee* list is kept even when the name is on the handle.

So the fix has two halves, and neither needs the type engine:

* **1.1** `members_of` should qualify a bare class spelling **before** answering, not after an empty answer — the
  arguments the caller wrote are kept (`shared_ptr<Resource>` → `std::shared_ptr<Resource>`), which is exactly what the
  member-list path already does for `direct_members`.
* **1.2** The hover should ask **both** classes for the name (pointee first, then written) and only then decide; today a
  list that merely *exists* wins over a list that *has the name*.

**What settles it:** `inspect complete` and `inspect hover` at the two positions, with the current binary; then
`docs/buglist.md` gets the numbers and the two gate tests (`completion::a_smart_pointers_own_members_are_completed`,
`reported_hover::a_smart_pointers_reset_is_the_handles`).

**1.0 — re-measure first.** The `inspect` binary in `target/release` may predate the last two commits (a stale binary is
what made `members_of` look wrong in the earlier session). Rebuild, then reproduce.

---

## 2. `DeclFact::type_of` loses its shape match — the root cause behind the pointee path

`docs/hover-render.md` §1 has the measurements: the field is recovered by matching a binding to a syntax shape by range,
and text **after** the declaration can make the match miss (`bits/shared_ptr.h`: present at 191 lines, gone at 197;
gone when the file's last token is an unterminated `/**`). When it misses, `element_type` has no type, so the arrow's
protocol fallback has nothing to read.

* **2.1** When the shape lookup misses, read the type from the binding's own `range` — it *is* the declaration's range,
  so the fallback cannot miss. This is a change in `declared_type_of_with`, one function.
* **2.2** Count the misses (`inspect stats`), so the bug is visible instead of silent.
* **2.3** Then re-check `std::shared_ptr<Resource>::element_type` on `test_cpp`; the two-file fixture already passes.

**What settles it:** a test that asserts `type_of` is present for the declaration in a file whose later text contains a
conditional block, and the same assertion for the file ending in `/**`.

---

## 3. The two readings — the design question that keeps the bug alive

`refinement.md` §5 item 4 states it: one reading per file, but two summaries (the file's own text, and the cooked
stream), and a query does not say which one it is reading. `inspect facts` reads the first, a hover may use the second —
which is why a fix verified in one can regress in the other.

* **3.1** Decide the rule. The cheap version: every query starts from a *fact*, and a fact carries the reading it came
  from (`env_hash`), so a follow-up question asks the same reading.
* **3.2** The cheaper version: make the two readers agree on the fields that are read (the `type_of` fallback in §2.1 is
  step one, because it removes the dependence on a shape match entirely).

**What settles it:** a probe that asks the same ten questions of both readings and prints where they disagree. That list
is the work.

---

## 4. Refinement — the three items that are already measured

From `refinement.md` §6, with item 1 and 2 done (closure-local candidates: 65 ms for a header in a 1 590-file index;
the cooked reading is stored and served: 48 → 12.8 ms a file):

* **4.1 Never serve a refinement from a request.** Drop `advance`'s tail from the pull path (`finish_a_slice` when
  idle); the pump serves the ask in one slice. A slow refinement then stops being a stalled server.
  *Measure:* `worst_write_hold` before/after.
* **4.2 Refine the open files' closures eagerly.** Affordable now at 45–65 ms a file, in the pump, bounded. The payoff
  is that the first hover on the open file answers from L2 instead of asking and waiting.
  *Measure:* hover `wait gate` + `wait session` on a cold start.
* **4.3 Retire `parsed_since_the_last_pass` as the ask-list.** It is 1 590 paths on a warm session and no longer means
  "work"; `re_read_backlog` stops lying.
  *Measure:* `MemoryReport` after a start.

---

## 5. The decorated constructor's missing fact

`docs/buglist.md` §30 fixed the diagnostics by asking the declaration's **name** (`promises_no_return`), and taught
`CppDeclaration::get_name` where `explicit A(int x)` keeps its name. The **scope walker** still uses its own reader
(`declared_name`), so a decorated constructor has **no fact**: the parameter beside it is bound (`2:20 x` in the
fixture), the constructor itself is not. Consequences: member lists do not offer it, completion after `A::` does not,
and `missing-return`'s fix is the only consumer that sees it.

* **5.1** `declared_name` should ask the same reader — or `get_name`'s new arm should be reachable from it — with the
  `is_unnamed_declaration` guard checked against call statements (`A(1);` has no specifier sequence, which is what keeps
  the fallback safe).
* **5.2** Then a member list includes the decorated constructors, and `signature_help` offers them at a construction.

**What settles it:** `inspect facts` listing the constructor, and a completion test after `A(` offering it.

---

## 6. The hover rendering's small gaps (each self-contained)

From `docs/hover-render.md` §3, in the order they are likely to be met:

* **`std::less<>` renders `std::less`** — `parse_type_spelling` drops an empty argument list, which changes the name.
* **A template-id callee answers nothing** (`std::make_shared<Resource>(2)`): the callee is not a plain name.
* **The prose still prints raw fact strings** (`returning \`std::vector<std::string> &\``) — route `kind_words` through
  `render::a_type`.
* **`the_type_the_macros_leave`** is a fact-level repair living in hover; move it to the fact builder.
* **Operator hovers hit the keyword vocabulary** (`operator bool`): the card should be the declaration.
* **Base access is not printed** (`: Base` where the clause says `public`).
* **Function-pointer parameter types** are the model's unparsed name.
* **`value` in a lambda is coloured `method`** (`test_cpp/src/smart_pointers.cpp:64`).

---

## The order I would take

1. §1.0 re-measure (30 minutes, decides whether §1 is a bug or a stale binary).
2. §1.1 + §1.2 with the two gate tests — the reported symptom, and the smallest change.
3. §2.1 + §2.3 — the root cause, and it makes §1's second half unnecessary in the end.
4. §5.1 — the fact gap the last fix exposed, and the reason completion does not see the constructor.
5. §3.1 — the design rule, with the disagreement probe as the artifact.
6. §4.1 + §4.2 — the measured refinements, once the answers above are stable.
7. §6 as they are met.
