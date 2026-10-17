# Overloads, template inference, and the checks that depend on them

## 1. What exists, read out of the code rather than assumed

```text
sema/overload.rs        **new this round** — the resolver (see §2)
sema/deduce.rs          `auto` only: what a placeholder declaration's initializer has. Not template deduction.
sema/types.rs           `Type::convertible_to(&self, target) -> Known<()>` — the crate's one answer to "can this go
                        there", and `parse_type_spelling(&str) -> Type`
sema/check/*.rs         14 codes. Two of them are about calls, and both refuse an overload set **by name**:
  argument_count.rs:191   "what a call resolves to is overload resolution: picking one would report an arity against
                           the wrong signature"
  param_type_check.rs:15  "a name that denotes an **overload set** is a call whose meaning is the language's
                           resolution, and a check with a rule of its own would be wrong about every overload it did
                           not pick"
```

Both refusals were correct while there was no resolver. There is one now, so the refusal can become a question.

## 2. The resolver, and exactly how much of C++ it is

`sema/overload.rs` — `choose(argument_types, candidates) -> Choice::{One(i), Ambiguous, None}`.

```text
viable      every argument fits its parameter (by `convertible_to`), and the arity works. A **longer** parameter list
            is viable for a shorter argument list: the extra parameters may have defaults, and defaults are not in the
            spelling this reads. `void f(int, int = 0)` must stay a candidate for `f(1)`.
ranked      Exact      the same type however written — `T`, `const T&` and `T&&` are one type as far as this goes
            Promotion  an integral promotion (narrow -> `int`/`unsigned int`) or `float` -> `double`, **and nothing
                       else**: `short` to `long` is a conversion, and the first version of this table said otherwise
            Conversion a real conversion: `int` to `double`, an array to a pointer, a derived pointer to a base one
            Unranked   the argument's type is `Unknown` — it neither wins nor loses, so it can only win alone
            NotViable  `Known::No`: the expression has no type at all
chosen      the candidate no other strictly beats, in every argument and better in one; ties are `Ambiguous`; among
            survivors, the one using **fewer parameters** wins, because the other needs a default argument
```

Deliberately absent, because a checker that guessed them would produce findings: user-defined conversions, reference
binding subtleties, and template candidates (which have to be deduced before they can be ranked — §4).

## 3. What the wiring changes, exactly

```text
argument_count      a name with several declarations is no longer skipped. Ask the resolver with the call's argument
                    types: `One` means the arity fits by construction (nothing to report), `None` means no candidate
                    takes this shape — report against the **closest** candidate so the message can say "too few" or
                    "too many" — and `Ambiguous` means say nothing.
param_type_check    resolve, then check the arguments against the **chosen** candidate's parameters. `Ambiguous` and
                    `None` say nothing: the first is the language's own answer, and the second has no argument to
                    blame without a closest-candidate rule, which is the next step after this one.
```

Both need the candidates *the checks can see*, which today is **this file's own functions** (`context.own_syntax_nodes`).
That is a real limitation and is stated rather than hidden: an overload set spread across a header and a source file is
still not resolved. Widening it means reading a declaration's parameters out of the **index** instead of a tree, which
is the measured problem `param_type_check.rs` documents (reading `DeclFact::parameters` produced 119 findings on 200
MSVC headers).

## 4. Templates, in the order they will be taken

```text
1. function templates       `template <typename T> void f(T)` — deduce `T` from the call's argument types, substitute,
                            then rank against the non-template candidates. A non-template that is equally good wins,
                            which is the language's rule and the one that makes `f(1)` choose `f(int)` over `f(T)`.
2. the checks that benefit  `param_type_check` on a chosen template candidate: its parameters are the *substituted*
                            ones, which is what a wrong argument should be compared with
3. what is already there    `declared_template_parameters_with`, variable templates, alias chains (§11 of
                            `walk-redesign.md`) — the declaration side of templates, which this will reuse
```
## 5. `param_type_check` is wired, and the wiring is **observably a no-op** — which is a finding

The check now builds every declaration of a name, dedupes a declaration and its definition, and asks the resolver for
the call's meaning; only the chosen candidate is checked.

Measured: the suite is unchanged (53 targets ok, one pre-existing red) **and no finding moved**, which is the right
answer and an informative one:

```text
a candidate is viable only if every argument is `convertible_to` its parameter
`param_type_check` reports exactly when an argument is **not** convertible to its parameter
⇒ a **chosen** candidate can never produce a finding
```

So resolving overloads cannot make this check report more, and that is not a defect in the resolution — it is the same
fact twice. What the resolution *does* buy here is the **removal of a refusal**: a name with several declarations is no
longer skipped, so a future rule that compares something weaker than `convertible_to` (a narrowing conversion, a
discarded `[[nodiscard]]`, a by-value copy) starts from a chosen candidate instead of from nothing.

**Where the resolution has real, observable value is elsehwere**, and it is the next two pieces of work:

```text
argument_count     an overload set is the case where the arity rule is wrong today: the check silently keeps the last
                   declaration of a name and reports against it. With the resolver, "no candidate takes this shape" is
                   the only reportable case, and "some candidate takes it" is silence — which is the language's rule.
hover / signature  the same `choose` answers "which of these five `operator<<` a reader is looking at", which is what
                   the objective's third clause asks for and what nothing in this crate can do today (`inlay.rs` says so
                   in as many words: "overload set nothing resolves").
```
## 6. `argument_count` is wired — and the fixture says it is not right yet

What changed (13 edits in `sema/check/argument_count.rs`, all mechanical once the anchors were exact):

```text
Callee              gains the parameter types, names dropped: `f(int n)` is `["int"]` for the resolver's sake
callees_in_this_file  a set per name, deduped by parameter list — a declaration plus its definition is one candidate
callees_from_the_index  every function fact of the name, not `[only]`, with the same dedupe
the call path       `One` -> that candidate (and the arity cannot fire against it), `None` -> the **nearest**
                    candidate so a message can still say "too many" or "too few", `Ambiguous` -> silence
```

**A process finding worth more than the wiring:** every multi-line `Replace` in this session that "MISSed" had one
cause — the source files are **CRLF** and the anchors were written with **LF**. Thirteen anchors that had failed
repeatedly all matched on the first attempt after one `-replace "`r`n", "`n"`. That is why four earlier rounds edited
by line number, and it is a cheap fix to a repeated waste.

**What is not right:** the regression test is written and `#[ignore]`d —
`an_overload_set_is_resolved_rather_than_judged_against_its_last_declaration`. On

```cpp
void f(int) {}
void f(int, int) {}
void g() { f(1); f(1, 2); }
```

`codes_of` still answers `[ArgumentCount]`, so either the candidate set this check builds for `f` is not those two
functions, or the rank is not `Exact` for both and the tie is broken differently than `choose` documents. The next
probe is one `eprintln!` of the map for this text — the same shape every other defect in this layer was found with, and
the reason the assertion was kept rather than deleted: it says what correct means.
## 7. The defect §6 left open, found and fixed

The probe was one `eprintln!` of the candidate set and the call, and it answered in one run:

```text
call `f` args 1 set [(1, 1, false, [])]        ← ONE candidate, and its parameter list is **empty**
call `f` args 2 set [(1, 1, false, [])]        ← so the two-argument call was judged against a one-parameter
                                                declaration: "takes 1 argument(s) and this call passes more"
```

One cause for both halves: the spellings were read with `super::spelling_of(&parameter)`, which is written for a
declarator or an expression and answers **`None` for a parameter node**. So every candidate had an empty parameter
list, and the dedupe — which compares parameter lists — threw the second overload away as a duplicate of the first.

Fixed by reading the node's own text (`parameter.text()`, then `a_type_without_its_name`). Measured after:

```text
call `f` args 1  set [(1,1,["int"]), (2,2,["int","int"])]  types [int]       -> One(0), judged against 1
call `f` args 2  set [(1,1,["int"]), (2,2,["int","int"])]  types [int,int]   -> One(1), judged against 2
call `f` args 3  set [(1,1,["int"]), (2,2,["int","int"])]  types [int×3]     -> None, nearest is 2 → a finding
```

`an_overload_set_is_resolved_rather_than_judged_against_its_last_declaration` is **green and no longer ignored**, and
the suite is back to **53 targets ok** with only the pre-existing latency test red. The probe is deleted.

**One intermittent failure was seen once and not again** — `a_completion_sees_a_change_to_an_included_header`, in the
handshake binary, which passes 3/3 on its own and passed on the next full run. It is recorded here rather than fixed:
it is a parallel-server test and nothing in this round's change touches the path it exercises, but a flake that appeared
the first time a check's resolution changed is worth remembering.
## 8. Hover and signature help: what already works, and what "picking" needs

Read before writing, and the reading changed the plan:

```text
hover               already lists every definition (`found.found.len() > 1` renders them with a count), and the sema
                    layer already signals `UnknownReason::Ambiguous` — so a set is not *silent*, it is a list
signature help      already sends **all** the signatures on purpose ("the alternative to a list is not one signature
                    that is right but one that is wrong four times out of five"), and the client's `activeSignature`
                    is what the reader cycles
CallSignature       label, parameters, active_parameter, declared_in, declared_at, documentation — and **no field that
                    says "this is the one the call means"**, and no instantiated label for a template candidate
```

So "pick the right overload" is not "replace a list with one item" — both layers are right to keep the list. What is
missing is the **choice inside the list**: which of these five signatures the call's own arguments select, so that
signature help can point `activeSignature` at it and hover can mark it.

The pieces, and the one that decides the shape:

```text
where the choice is computed   `Session::signatures_at` — it already builds every signature and already holds the view
                               and the index; like every query here (`check`), not `signature.rs`, because typing an
                               argument needs `sema::model::SemanticModel` and `Session` is what builds one
what it needs                  the callee's name (the call is already found there), `index.definitions(name, path)`
                               for the candidates with their `parameter_list` facts, and `model.type_of` for each
                               argument — the same three inputs `argument_count` uses
how the answer is carried      `CallSignature` gains no field: the list is **ordered** with the chosen one first, and
                               the handler sets `activeSignature` to the first. A field would be more explicit and is
                               the alternative if the order turns out to matter to a client — written down because the
                               choice affects what the reader cycles through
what a template needs          the label must be the **substituted** one: `f<int>`'s signature, not `f<T>`'s. That is
                               clause 3 of the objective, and it is why the ordering decision can wait until the
                               deduction exists — the label is going to change anyway
```
## 9. Signature help now *chooses* — and the list is still whole

`signature::signatures_at` ranks the signatures it is about to return with the same `choose` the checks use — the call's
arguments typed by a `SemanticModel`, the candidates' parameter spellings from the tree it already had — and moves the
chosen one to the front. The handler already answered `active_signature: Some(0)`, so the front of the list **is** what
a client shows first: no field was added, and nothing else changed.

```text
void print(int value);
void print(const char* text);
print(1)      → "print(int value)" is the first signature      (and active)
print("x")    → "print(const char* text)" is the first signature
```

Both directions are a test (`signature.rs::the_overload_the_arguments_select_is_the_first_signature`), because a
resolution that always picked the first candidate would pass the first half and fail the second.

**What is deliberately not done**: the other signatures stay in the list. A client lets the reader cycle, and dropping
them would replace "sometimes the wrong one first" with "the other four are unreachable" — the trade `signature_help.rs`
already documents in its own words.
## 10. Function template deduction — the module, its eight tests, and what the wiring still needs

`sema/deduction.rs` answers "what is `T`, given the call", in the form the rest of the crate already speaks:
a `Vec<String>` in the order the template declares its parameters, which `sema::types::substituted_spelling` turns into
the parameter types the call is really facing.

```text
deduced   `T`                  the parameter is exactly the parameter
          `const T&` / `T&&`   the same type seen through a reference and a cv-qualifier
          `T*` / `T&` / `T[]`  a shape the argument has and the parameter names around it
          `std::vector<T>`     one level, by what surrounds the name — measured, and the first version of its test
                               asserted a refusal here, which was wrong about what this can do
          `T` twice            if both agree, once; if they disagree, **not at all**, because that call does not compile
refused   an unbound name    `f(T*)` with `int` is not a deduction
          a parameter that     `f(int)` deduces nothing
            names no name
          a shape the          `std::vector<T>` with `std::map<int, int>`
            argument lacks
          anything else        `T::value_type`, a pack — each needs a unifier this does not have, and refusing gives
                               the resolver an `Unranked` candidate instead of a substitution wrong for some calls
```

Eight tests, all green (`sema::deduction::tests`), and the suite is at **53 targets ok** with the one pre-existing red.

**What the wiring still needs, read rather than assumed:** `DeclFact` has **no** template-parameter field, so a
candidate's `<typename T>` header has to be read from the tree — the same place `signature.rs` already reads each
candidate's `parameter_list` from (`parameter_list_of(declared_in, name_offset)`), one step up from the name to the
declaration that owns the `template <...>` clause. Then:

```text
signature.rs   for a candidate whose declaration owns a template header: deduce from the call's argument types, then
               `substituted_spelling` every parameter spelling before ranking, so `f<int>` is what the ranking sees and
               what the label shows
overload       a template and a non-template that rank equally: the **non-template** wins — `f(int)` beats `f(T)` for
               `f(1)` — which is a rule the resolver does not have yet and needs before templates are mixed in
```
## 11. The deduction is wired — and the fixture says the *callees* are the gap, not the ranking

`signature.rs` now keeps, per signature, the declaration's parameter spellings and the `template <...>` names before
it; once the call's argument types are known it deduces, substitutes with `sema::types::substituted_spelling`, and
ranks the result. A name the call does not fix leaves the declared spellings, which the resolver reads as `Unranked` —
quiet, and right: a template this cannot deduce is a template whose ranking it does not know.

The test that was written to prove it — two templates, `take(T*)` and `take(T)`, called with `&n` — cannot run yet,
and what it says is worth more than a passing test:

```text
signature_where_marked(two templates + take(&n))   → **None**: there is no signature at all
```

So `callees_of_a_call` does not reach a **function template** — the resolution never gets a candidate to rank. That is
one layer earlier than the wiring this round added, and it is the real content of the objective's third clause: a
template is not in the index's `Function` declarations the way a plain function is, or `parameter_list_of` does not
answer for its name, and either is what has to be fixed before "deduce, substitute, use the result for checks" can be
measured end to end.

Two things were left behind on purpose, both recorded rather than half-built:

```text
the template header      read from the **text** before the declaration (`template_parameters_before`), because
                         `DeclFact` has no template-parameter field and no tree reader is reachable from a signature.
                         It is a heuristic and says so; a `DeclFact` field would be the structural version
the non-template rule    `f(int)` beats `f(T)` for `f(1)` when both rank equally. `Candidate` has no "is a template"
                         flag, so a tie is `Ambiguous` today — safe, and not the language's answer
```
## 12. Deduction proven end to end — and §11's diagnosis was wrong

§11 said "`callees_of_a_call` does not reach a function template". That was a **wrong hypothesis recorded as a finding**,
and the probe that settled it took two lines:

```text
call_around(take(|&n))            → **None**: there is no call node at all
candidates for `take`…            ← only printed once the argument was a pointer variable
  candidate at 27: parameter_list_of Some("(T* value)")
  candidate at 70: parameter_list_of Some("(T value)")
```

So the callees were reachable and the parameter lists were found; the fixture simply had no call in it, because
**`&n` as a call argument produces no call node** — a parser defect of its own, unrelated to templates, and one that
would make `take(&anything)` invisible to every query that starts from a call. With `int* p = &n; take(p);`:

```text
signature.rs::a_template_candidate_is_ranked_as_the_function_the_call_instantiates   ** passes **
```

The distinguishing shape is worth keeping in mind: two templates, `take(T*)` and `take(T)`, called with an `int*`. With
deduction the first ranks as `int*` (exact) and the second as `int` against an `int*` (not viable), so the `T*` overload
comes first; without it both are `Unranked`, the call is ambiguous, and nothing is chosen. The test fails if the
deduction silently stops.

**Left open, both named:**

```text
`&n` as an argument     a parser defect: `take(&n)` yields no call node, so no signature, no hover, no check. Nothing in
                        this round's change caused it and nothing in it is fixed by this round
the non-template rule   `f(int)` beats `f(T)` when both rank equally; `Candidate` has no "is a template" flag, so a tie
                        is `Ambiguous`. Safe, and not the language's answer
```
## 13. The non-template rule, and the second wrong "template" this round caught

`Candidate` now says whether it is a template, and `choose` applies the language's tie-break: among the candidates
nothing beats, an **ordinary function** wins over a template that is equally good, so `f(1)` means `f(int)` when
`template <typename T> f(T)` is also in view. Two templates are still a tie and a tie is still `Ambiguous`.

The first attempt at the end-to-end test failed with `pick(T value)` first, and the cause was in the **header reader**,
not the rule: `rfind("template")` finds the nearest preceding `template` for an ordinary function declared *after* a
template too, so the ordinary function was marked as the template and the tie-break that prefers it never ran. Fixed by
requiring that no `;` falls between the header's `>` and the name — a declaration that ended in between means the header
belongs to somebody else.

```text
template <typename T> void pick(T value);
void pick(int value);
int g() { pick(5); return 0; }
→ the first signature is `pick(int value)`   (the template is written first, so declaration order would have said
                                              otherwise)
```

That is twice in two rounds that a "template" was misidentified by a text heuristic — first a `&n` that was not a call,
now a header that was not this declaration's. Both are recorded, and both point the same direction: the structural
version — a `DeclFact` field for the template header — removes the whole class.
## 14. `take(&n)` is not a call — measured, diagnosed, not yet fixed

The `&n` defect §12 recorded is confirmed at the parser level and its cause is in the tree:

```text
void g() { int n = 0; take(&n); }     →  0 CallExpr, 0 errors, round-trips exactly

Syntax(Declarator)
  Syntax(ReferenceType) → Token(Ampersand) "&"
  Syntax(NameExpr)      → Token(Identifier) "n"
```

It is the **most-vexing-parse** shape: this grammar permits any identifier to start a type, so `take(&n);` reads as the
**declaration** of a reference named `n` of type `take`. Nothing downstream can see a call in it, which is why signature
help, hover on the callee and both argument checks were blind to the most ordinary line there is — and why this looked
like a template problem for a whole round (`take(p)` works, `take(&n)` does not).

**The fix is one rule in the statement parser**, written down here rather than rushed into a parser at the end of a
round: when the declaration reading's `Declarator` has a `ReferenceType` as the **first token inside the parentheses**,
the program wrote a call, and the expression reading is the right one. `void f(int& x);` is unaffected — there the `&`
follows a type, not the `(`.

The test is `expressions.rs::a_call_whose_argument_is_an_address_of_expression_is_a_call`, `#[ignore]`d with the tree in
its note. Measuring first was worth it: the defect looked like "templates are not found", and it was a parse.
## 15. `take(&n)` is a call — the one-rule fix, and the lookahead mistake inside it

The statement rule §14 wrote down is in `parse_declaration_or_expression_statement`, before the speculative
declaration pass:

```rust
// identifier, then `(`, then `&` is a call whose first argument is an address
if a_call_whose_first_argument_is_an_address(p) {
    return parse_expression_statement(p);
}
```

Exact, and it costs nothing else: `void f(int& x);` is not matched (the `&` follows a type, not the parenthesis), and
the rule only runs where a statement begins.

**The first version of the lookahead did not fire, and the reason is worth keeping:** `token_kind_at` is an *absolute*
token index, so asking for tokens `0, 1, 2` asked about the first three tokens of the **file** — `void`, `g`, `(` — and
never matched. The tree still showed a `Declaration`, which is what said the rule had not run rather than run and
failed. Based on `current_token_index()` it works:

```text
expressions.rs::a_call_whose_argument_is_an_address_of_expression_is_a_call   ** passes **
full suite                                                                    53 targets ok, the one pre-existing red
```

That is the third defect in four rounds found by printing what the parser produced instead of reasoning about what it
should produce — and the second where the *probe* was right and the *fix's first attempt* was wrong.
## 16. Hover marking: written, and it does not fire yet

`overloads_markdown` now asks the resolver which declaration a call means and prefixes it with `→`; the sentence under
the block changes when there is one. The answer is read from `Session::signatures_at`, which already orders its
signatures with the chosen one first.

Measured with the marking in: the popup is **unchanged**, old sentence included — so `signatures_at` answered nothing at
the offset the hover passed. Two offsets were tried:

```text
the callee's own offset        `signature.rs::a_cursor_on_the_callee_has_no_signature` already says this answers nothing
the call's `(` plus one        also nothing — which is what is left to explain
```

The test is written, `#[ignore]`d and keeps its assertion; the code is inert until the question is answered, and the
next probe is one `eprintln!` of the call node the hover token belongs to and the offset the query is given. Everything
else in this round's file is unchanged: the suite is **53 targets ok** with only the pre-existing latency red, and the
`&n` parser fix from §15 is in and green.
## 17. What the last probe measured, and the one thing it did not

The hover token's offset **is** inside the call — traced through the server's own log, which is the only channel that
survives the harness (an `eprintln!` goes to a stderr nobody keeps; `log::info!` with `CPPLS_TEST_LOG_LEVEL=info` lands
in the file the failure message names):

```text
hover-trace: offset 17 token Token(Identifier) ancestors 6
hover-trace: found a call "parse(1)"
```

So `token_at` → `parent_ancestors` → `CallExpr` works, and the call is `parse(1)`. What did **not** happen, in two
attempts, is `Session::signatures_at` being reached with an offset it answers for:

```text
the call's `(` found by walking its tokens     the walk answered `None` — the query was never called
the call's last byte (`end() - 1`, the `)`)     reached the query, and it answered nothing
```

That is where this stops: the next measurement is the one thing not yet traced — `call_around`'s own answer **for the
offset the query is given**, from inside `signature::signatures_at`, with `CPPLS_SIGNATURE_TRACE=1` set on the server
process. Every other possibility in this chain has been eliminated by a trace rather than by reasoning.

The hover code is inert until that answers (no `→` is drawn, the popup text is unchanged), the test is `#[ignore]`d and
keeps its assertion, the suite is **53 targets ok** with only the pre-existing latency red, and the traces are removed
from `hover/mod.rs`.
## 18. The chain is traced end to end, and the empty answer is the **index**, not the query

The one measurement §17 left, taken through the server's log:

```text
signatures_at: offset 24 of 28 bytes in …\main.cpp, call Some("parse(1)")
```

`call_around` answers — the call is found, at the offset the query is given. So the empty signature list comes **after**
it, and the shape of the fixture says where: `parse` is **declared in `overloads.h`** and called in `main.cpp`, and a
hover does not prepare the declaring file the way it prepares the hovered one. `callees_of_a_call` therefore reaches
`index.definitions("parse", main.cpp)`, the header has not been read, and the answer is `Unknown` — an empty list, not an
empty *result*.

That also explains why the signature **popup** works in practice and this did not: a reader who is typing a call has the
header open or the index already warm, and every handshake test for signatures retries until the index catches up. This
test's `hover_over` retries until the hover answers at all — and the overload *list* answers immediately, so the retry
stopped one step too early.

**Two ways to make the mark appear, both recorded rather than half-built:**

```text
prepare the declaring files   the hover handler would call the same `prepare` it already calls for the hovered file,
                              once per declaration — async, and bounded by the overload count
rank locally from the facts   `overloads_markdown` already holds every declaration's `parameter_list`; what it lacks is
                              the argument types, which need a `SemanticModel`, which only `Session` builds. A `Session`
                              query that answers "which of these facts does this call mean" would be the clean version —
                              the same shape as `signatures_at` but keyed by facts rather than by parsed signatures
```

The hover code stays inert, the test stays `#[ignore]`d with its assertion, the trace is removed, and the suite is
**53 targets ok** with only the pre-existing latency red.
## 19. Hover marks the chosen overload — and the fix was to stop asking the wrong question

§18 left two ways to make the mark appear. The one taken is the second, and smaller: a `Session` query that resolves the
call against the **declarations the caller already holds**, so nothing has to be parsed as a signature source and the
index does not have to have read the header.

```sema::overload::parameters_in   `(int a, const char* b = 0)` → `["int", "const char*"]`, and `...` → variadic
                                    the text half of a `parameter_list` fact, split at a depth-zero comma and with
                                    the default and the parameter's own name dropped
Session::the_declaration_a_call_means(view, offset, parameter_lists) -> Option<usize>
                                    finds the call the offset is in, types its arguments with a `SemanticModel`, ranks
                                    the facts, and answers which one — `None` for not-a-call, nothing viable, ambiguous
hover                             marks that declaration with `→` and says so under the block
```

Measured: the hover test is **green and no longer ignored**, and the suite is **53 targets ok** with only the
pre-existing latency red. The build is clean.

**Why this shape is also the right one for the checks:** the same query answers from facts rather than from parsed
signatures, so a check that has declarations in hand (as `argument_count` and `param_type_check` both do) can ask it
without a second parse — and that is the missing half of "deduce, substitute, and use the result for checks": the
substituted spellings can be ranked through this door, not only through the signature popup's.
## 20. What deduction does **not** buy the checks, measured rather than assumed

The third clause says "deduce, substitute, and use the result for checks". The first half is built (§10, §12) and the
second half, for the two argument checks, turns out to buy **nothing** — and the reason is structural, not an omission:

```text
`f(T)` called with a `std::string`    deduction says T = std::string
                                      substitution makes the parameter `std::string`
                                      and a parameter that *is* the argument's own type is convertible by construction
⇒ `param-type-check` can never report on a plain `T` parameter, deduced or not
```

A parameter that is **not** deduced (`template <class T> void f(T a, int b)`) is checkable today and needs no deduction:
its spelling `int` is already the declaration's. So the check that a template changes is the **arity** one, which does
not look at types at all — and it already judges templates (`written` and `required` come from the declaration's own
tokens, which §«interval»/`has_an_ellipsis` made right long before this work).

That is now a test rather than a paragraph —
`checks.rs::a_templates_own_parameter_is_silent_to_the_type_check_and_the_arity_check_still_judges_it`: `take("one")`
answers nothing, `take(1, 2)` answers `ArgumentCount`.

**What is genuinely still missing** is on the display side, and it is the visible half of "template inference": hover
and the signature popup show the **declared** spelling, `take(T* value)`, rather than the instantiated `take(int* value)`
— the reader cannot see that the analysis deduced anything. The pieces exist:

```text
the deduced arguments   `sema::deduction::deduce(names, parameters, types)`
the substitution        `sema::types::substituted_spelling(written, names, arguments)`
the names               the one weak link: they come from `template_parameters_before`, a **text** heuristic, because
                        `DeclFact` has no template header — the same note §13 and §15 ended on
the spans               the label is bytes with client-visible spans inside it, so a substituted label has to be
                        rebuilt rather than edited in place
```
## 21. The display side is in, and the same absence blocks it a third time

What was built: the template-header reader moved from `signature.rs` into `sema::deduction` (one copy for both layers),
`Session::the_deduced_arguments` (the call's argument types plus a declaration's names and parameters, answering the
template arguments), and hover now renders the **chosen** declaration's line with those arguments substituted —
`take(T* value)` becomes `take(int* value)`.

What was measured: for a set of two **templates** the marking does not appear at all, because
`the_declaration_a_call_means` ranks the facts' `parameter_list` spellings — `T*` and `T`, both `Unranked` against an
`int*` — and a tie is `Ambiguous`. It has no template names to deduce with, and it cannot read them: they are in the
declaring file's **text**, and a fact does not carry them.

```text
consumer                     needs                today
signature.rs                 the header from text a text heuristic (§13, §15)
the argument checks          nothing (§20)        —
the fact-based query (hover) the header            **blocked** — no facts, no deduction
```

That is three consumers, one absence: **`DeclFact` has no template header.** The structured fix is a field on the fact
(the cook already reads the declaration's text and its `parameter_list`; the `template <...>` clause is in the same
place), and it would retire the heuristic in `deduction::template_parameters_before` rather than add a second reader
beside it.

Both new tests are recorded: the fact-based marking for a **non-template** set passes
(`the_overload_a_call_means_is_marked_in_the_hover_list`), and the template one is `#[ignore]`d with this note. Suite:
**53 targets ok**, one pre-existing red, build clean.
## 22. The third consumer is unblocked, and the tie is **partial ordering** — a rule not in the resolver

The names now travel: `the_declaration_a_call_means` takes a parallel `&[Vec<String>]` of template parameters, deduces
each candidate that has them, substitutes, and marks it `as_a_template` — and hover reads the names from each declaring
file's text, which is the one thing a fact does not carry. Traced end to end:

```text
types [Yes("int*")]   candidates [(["int*"], true), (["int*"], true)]
```

**Both candidates deduced correctly** — `T*` and `T` from an `int*` are both `int*` when substituted — and my earlier
hypothesis that one of them failed to deduce was wrong. They tie, both are templates, so the non-template rule of §13
does not apply, and the answer is `Ambiguous`. The language's answer is that **`take(T*)` is more specialized than
`take(T)`**: the tie is settled by **partial ordering of templates**, which the resolver does not have.

That is the next rule, stated so it can be implemented rather than rediscovered:

```text
more specialized    `T*` beats `T`, `vector<T>` beats `T`, `T&` beats `const T&` for a non-const argument — decided by
                    deducing each template's parameters from the *other's* parameter type, without ever naming an
                    actual argument type
not it              anything about the call's argument — partial ordering is a property of the two declarations
```

The display-side substitution stays in and inert, the test stays `#[ignore]`d with its assertion, the traces are
removed, and the suite is **53 targets ok** with only the pre-existing latency red.
## 23. Partial ordering: the last rule, and the template path is complete

`sema::overload::more_specialized(mine, theirs)` decides the tie the language settles by **partial ordering**, with no
argument type anywhere in it: `mine` is more specialized when `theirs`'s parameter spellings can be deduced against
`mine`'s and not the other way round. `Candidate` carries what the declaration **wrote** (`declared`) beside the
substituted spellings the ranking uses (`parameters`), and `choose` applies the rule after the non-template rule of §13
and before the parameter-count rule.

```text
take(T*)  vs  take(T)  called with `int*`
  before this rule   both substitute to `int*`, both are `Exact`, both are templates ⇒ Ambiguous, nothing marked
  with it            `T*` is more specialized ⇒ One(0) ⇒ hover marks it and shows `take(int* value)`
```

Measured: `overload.rs` has **14 tests** (the new one checks the relation is not symmetric, that it picks the pointer
template, and that two identical declarations stay ambiguous), and both hover tests that were `#[ignore]`d now **pass**
— `the_overload_a_call_means_is_marked_in_the_hover_list` and `the_chosen_template_overload_is_shown_instantiated`.
Suite: **53 targets ok**, one pre-existing red, build clean.

**Where the objective stands after this:**

```text
(1) paths understood        §1-§4, §8
(2) overload resolution     resolver + four consumers: both argument checks, the signature popup, hover
                            — go-to-definition still answers the *set* (the protocol allows a list); ordering its
                              first entry would mirror hover and is the one place left
(3) function templates      deduce, substitute, rank, partial ordering, show instantiated
                            — "use the result for checks" is measured to be a no-op and pinned by a test (§20)
(4) suite and docs          53 ok / 1 pre-existing red, build clean, this file
```
## 24. Go-to-definition leads with the chosen overload — the last consumer

`handlers/definition` sorted its locations so the one the call selects is **first**, using the same two queries hover
marks from (`Session::the_declaration_a_call_means` with the facts' parameter lists and the template names their files
carry). The sort is **stable**, so every other location keeps the index's order; a name with one declaration, a cursor
that is not on a call, and a call nothing selects all answer exactly as before.

Measured — `the_definition_of_a_call_leads_with_the_overload_it_means`: three overloads, one per line of the header, and
`parse(1)` leads with **line 0** (`int parse(int value);`) instead of whichever the index happened to hold first.

## The objective, complete

```text
(1) paths understood        §1-§4, §8: the walk, the index, `definitions_at`, `callees_of_a_call`, `parameter_list_of`,
                            and the two checks' original refusals quoted where they stood
(2) overload resolution     `sema::overload`: viability, five ranks, the partial order, Ambiguous vs None, the
                            non-template rule, partial ordering. Four consumers, each with a test:
                              argument-count      an overload set is resolved, not judged against its last declaration
                              param-type-check    a set is no longer skipped (measured: no finding moves, §5)
                              signature help      the chosen overload's signature comes first, both directions tested
                              hover               the chosen one is marked `→` and shown **instantiated**
                              definition          the chosen one is the first location
(3) function templates      `sema::deduction`: deduce, substitute, rank, `partial ordering`, show instantiated.
                            "Use the result for checks" is measured to be a no-op and pinned by a test (§20) — a `T`
                            parameter is the argument's own type, so a type check on it can never report.
(4) suite and docs          53 targets ok every round, the one pre-existing latency red, build clean, §1-§24 here
```

**What made this work, in one line each:** a probe before every fix (four rounds of them changed the diagnosis), the
one-second single-test loop instead of the two-minute suite, the CRLF discovery that unblocked thirteen edits at once,
and the habit of writing down what a wrong hypothesis *was* when the next measurement refuted it.
