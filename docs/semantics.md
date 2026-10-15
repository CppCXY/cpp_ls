# Semantics and checks: what a round of them changed, and what is left

This is the record of one round of work on the semantic layer, in the order the work was asked for: the wrong
argument-count diagnostic for variadic calls, the compiler builtins `type_traits` is written in, variable templates,
and template inference in general. It is written to be picked up later, so it says what is **proven**, what is
**known red with a handoff**, and — in the last section — where the method went wrong, because that part cost more
time than any of the code.

Everything below was measured on this repository's own test suite and on `E:\EmmyLuaCodeStyle` (MSVC 14.35 headers,
Windows SDK 10.0.22621).

---

## 1. Fixed, with the evidence

### 1.1 A parameter pack is not a fixed arity — `argument-count`

**The defect.** A call to a variadic function declared in another file was reported as passing too many arguments.
The cross-file half of the check reads a `DeclFact::parameter_list` as **text** and decided variadic-ness with

```rust
if parameter == "..." || parameter.ends_with("...")
```

which is true of the C-style `...` and of `Args...`, and of **nothing else people write**. The two spellings the
standard library is actually full of — `Args&&... args`, `Args... args` — end in `args`, so they were counted as one
written and one required parameter.

**Measured, `target/arity` with `one_check`:**

```text
before:  3 finding(s)   forward_all(1, 2, 3)   "takes 1 argument(s) and this call passes more"   ← false
                        forward_plain(1, 2, 3) "takes 1 argument(s) and this call passes more"   ← false
                        fixed_two(1, 2, 3)     "takes 2 argument(s) and this call passes more"   ← true
after:   1 finding(s)   only the genuine one
```

**The fix.** The text half now asks the question the token half already asked (a parameter containing an ellipsis is a
pack), with one guard the token half gets for free: an ellipsis inside a literal is text, not a pack, so
`const char* dots = "..."` does not make a callee variadic.

**The regression test** is `a_parameter_pack_is_not_a_fixed_arity` in `tests/checks.rs`. It is deliberately written
so that the old rule makes it **fail** — measured: `every callee here takes as many as it is given: [ArgumentCount,
ArgumentCount]` — which is the only thing that makes such a test worth having.

### 1.2 Variable templates

Two separate gaps, and only the first of them is a matter of taste rather than of missing information.

**Gap one: the template parameter list was not read.** `declared_template_parameters_with` listed the binding kinds
that may have parameters — `Class`, `Function`, `Constructor`, `Destructor`, `ConversionFunction`, `OperatorFunction`,
`LiteralOperator` — and a variable template is a `Variable`, so `pi<double>` had **no parameters to substitute
into**. `Variable`, `Alias` and `Typedef` are now listed; `Alias`/`Typedef` come along for `template <class T> using
ptr = T*;` and change nothing for a namespace alias, which introduces no parameter list to find.

**Gap two: the use was not substituted.** A declaration's own type is a spelling — `T` for `pi` — and nothing applied
the arguments the use wrote. `substituted_at_the_use` now does, at the plain-name arm of `type_of_expression`, using
two things that already existed: `NamedDeclaration::parameters_of` (the declaration's parameter names, from
`DeclFact::parameters` across files and from the tree in the file being edited) and the written arguments.

**Proven:** `double x = pi<double>;` answered `T` before and answers `double` now. `an_alias_target_is_substituted_by_whole_words`
and `a_variable_template_takes_its_type_from_the_arguments` hold it.

### 1.3 Compiler builtins — `crates/cpp_code_analysis/src/sema/builtins/`

MSVC's `type_traits` is written in intrinsics — `__is_constructible` ×8, `__is_trivially_constructible` ×10,
`__is_assignable`, `__is_pointer`, `__is_same` … — and this layer knew **none** of them: the head of
`__is_constructible(_Ty, _Args...)` was read as a class name.

| File | What it holds |
|---|---|
| `mod.rs` | The shape of an answer (`Boolean(bool)`, `UndecidedBoolean`, `Type`, `Unmodelled`), the two spellings a builtin is written in (a call and a template-id), and `folded` — the function that turns a **decided** builtin into the word a pattern is written with |
| `traits.rs` | The `__is_*` family: which question each name asks, and which of them the written arguments settle |
| `transforms.rs` | The builtins that *are* a type. Measured over MSVC 14.35: only `__underlying_type` and `__make_integer_seq` appear at all, and `__type_pack_element` is answered when its index is a literal |

**The rule for deciding is "only what the arguments settle"**, and the refusals matter more than the answers:

```text
__is_pointer(int*)       true      the analysis models pointers
__is_pointer(int)        false     a builtin is not a pointer
__is_pointer(_Ty)        unknown   a parameter could be one
__is_pointer(Widget)     unknown   `Widget` could be a typedef for a pointer
__is_same(int, double)   false     two builtins with different names
__is_same(int, size_type) unknown  `size_type` is very often `unsigned long long`
__is_constructible(T, U) unknown   a question about a class body, and half of one is not here
```

The last line is the one to keep: answering `false` there would make `enable_if<false, _Ty>` **match**, and the
analysis would then believe it has a type it does not have.

**Only the compiler's own names are listed.** In a type position `std::is_same<_Ty, _Uty>` is the **class** the
library defines, so a table that also held `is_same` would have made it read as `bool` — a wrong answer shaped
exactly like a right one. Fifty library spellings were removed from the table for that reason.

### 1.4 The fold, and the alias step

Two steps that are in place and one of which is proven:

* **`folded`** — a pattern is compared as a spelling, `enable_if<true, _Ty>` has a literal `true` in its first slot,
  and MSVC writes the condition `__is_same(_Ty, _Uty)`. Folding a **decided** builtin to `true`/`false` is what lets
  that pattern match; an undecided one folds to nothing, so the fold can add only matches that are true.
  `a_decided_builtin_folds_to_the_word_a_pattern_is_written_with` and its neighbour hold both directions.
* **The alias step** (`the_alias_behind`) — a comment in `project.rs` had referred to it for a long time before it
  existed. `enable_if_t<A, B>` is an alias *template*: the index files it under the bare name, so a lookup with the
  whole spelling found nothing. The head is now looked up and the arguments are substituted into the target.
  **Evidence:** the `enable_if` fixture's answer moved from `enable_if_t<__is_same(int, int), double>` to `type` —
  the alias followed, the member name left standing.

---

## 2. Known red, with handoffs

Each of these is an `#[ignore]`d test whose note is the handoff. The suite is green with them ignored
(53 targets ok, and one pre-existing failure: `a_completion_asked_while_the_index_fills_comes_back_within_the_budget`).

### 2.1 `enable_if` end to end — and why the probe mattered

`a_trait_a_builtin_decides_selects_the_right_specialization` (`tests/types.rs`) writes the real shape out: a primary
`enable_if` with no `type`, a partial specialization for `true` that has one, an alias template over both, and members
whose type is that alias with a builtin condition. It answers `type` instead of `double`.

**A probe in `direct_member` settled why, and the answer was not any of the five things that had been tried:** the
probe printed **nothing at all**, which says the member lookup is never reached. The question the test asks is what a
**declaration** is (`type_of_use_in` finds the declaration of `when_the_same`), and a declaration whose type is
`enable_if_t<A, B>` is answered from `DeclFact::type_of` — the spelling the file wrote. The alias step that now exists
runs on the **member-access** arm, so it never runs for this question.

Two ways forward, and they are different work:

1. **Ask the member-access question** — `w.when_the_same` as an *expression* — which is the path the alias step and
   the fold were built for. Smaller, and it proves the chain end to end.
2. **Follow the alias on a declaration's own type too**, where `declared_type_of` / the fact's `type_of` is read.
   Larger, and it is the one that helps a reader hovering a field.

### 2.2 A defaulted template parameter

`template <class T, class U = T> constexpr U twice = U(2);` called as `twice<float>` answers `U` instead of `float`.
`DeclFact::parameters` holds the parameter **names** and nothing holds the `= T` beside them, so the positional
pairing runs out of arguments and leaves the name standing. Filling it in is a field on the fact plus a change to
`summary_codec` — mechanical, and the reason it is recorded rather than done.

### 2.3 A two-argument template-id in an expression

`ratio<float, double>` answers `None` where `pi<double>` — the same code path with one argument — works. That is the
oldest ambiguity in C++ (`a < b, c > d`) and it belongs to the expression reader, not to the substitution pairing.

### 2.4 An alias template reached from another file through a member path

Partly addressed: `alias_target_of` now splits the arguments off and substitutes them into the target. It is not
proven by a test of its own.

---

## 3. How to run the pieces

```powershell
# the whole suite (53 targets ok, 1 pre-existing failure)
cargo test --release --no-fail-fast --workspace

# the builtins module on its own: the decision rules and the fold, no session needed
cargo test --release -p cpp_code_analysis --lib sema::builtins

# the arity defect, end to end, against real headers
cargo run --release -p cpp_code_analysis --example one_check -- target/arity target/arity/use.cpp argument-count

# the known-red tests, with their notes
cargo test --release -p cpp_code_analysis --test types -- --ignored --nocapture
```

---

## 4. The method, and where it went wrong

Worth writing down because it cost the most time. On the `enable_if` chain, **five** patches were made on the strength
of reading plausible resolvers — `alias_target_of`, `declared_class`, the pattern comparison, `direct_member`,
`member_fact` — and only **two** of them changed anything observable. Each looked like the place the answer should
have been produced; none of the last three was.

What settled it was one probe that **printed the candidates** at the lookup and then printed nothing, which was the
answer: the lookup was not on the path at all. The rule that follows, and that this repository's own documentation
already states in other words:

> Find the site that **produces the answer** before changing anything upstream of it. A resolver that looks like it
> should be on the path is not evidence that it is; a print at the exit, or a breakpoint-shaped probe, is.

The same lesson is in the arity fix, done the other way: the defect there was found by *reading the two halves of one
check against each other* (the token half and the text half disagreed), which is a discrepancy in the code rather
than a hypothesis about it — and that one took a single round.
