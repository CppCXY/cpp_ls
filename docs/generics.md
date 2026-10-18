# Generics and members: what a real editor session found

A reader reported three symptoms from one editor session. They were three layers of one chain, and the chain is worth
writing down because each layer was individually correct.

```cpp
auto z = std::vector<int>();
z.push_back(1);      // hover: 9 × basic_string::append, mentioning nothing about a vector
z.append(2);         // (the same popup)
std::string x = 123; // no diagnostic at all
```

## 1. `auto` over a type-id had no type — fixed

`type_of_expression` had arms for literals, `new`, `this`, names and **calls**, and none for a **functional cast of a
type**: `std::vector<int>()` is a call expression whose callee is a type-id, so it fell through every arm and answered
`None`. With `z`'s type unknown, the member lookup on `z.append` had nothing to resolve against and fell back to the
**name** query — which is what produced nine `basic_string::append` declarations in a popup about a `std::vector`.

The arm added to `type_of_expression`:

```rust
// A functional cast of a template-id is that type: std::vector<int>() has the type std::vector<int>, arguments kept.
if kind == CallExpr
    && let Some(callee) = written.strip_suffix("()")
    && callee.contains('<')
    && !callee.chars().any(char::is_whitespace)          // a type-id is one spelling, not `_STD h<int>`
    && index.files_declaring(bare, path).iter().any(|d| d.fact.kind == DeclKind::Type)
{ return Known::Yes((Type::named(callee.trim()), path.to_path_buf())); }
```

Each of the four conditions is a test that failed without it:

```text
callee.contains('<')            MAKE_T<int>() — a macro call — was claimed as a type
no whitespace                   `_STD h<int>()` — a macro *and* a name — was claimed as a type
the bare name is a Type         `h<int>()` — a **function template** call — was claimed as a type, shadowing the
                                call path that answers `h`'s return type
```

Tests: `types.rs::an_auto_variable_takes_the_templated_temporarys_type` and `…_the_calls_return_type`.

## 2. A hover on a member never asked about the object — fixed

`hover()` asked the **name** question and, when the name was ambiguous, listed every declaration of that spelling. For a
member access that is the wrong question: `z.append` is a question about `z`'s type.

The handler now starts with `member_access_at`, and answers one of three ways:

```text
the object's type is known, the member is on it      → the member's own declaration
the object's type is known, the member is not on it  → "`std::vector<int>` has no member `append`"
the object's type is not known                       → "the type of `z` is not known here" and why that usually is
```

The third is the one that matters for trust: the previous behaviour listed nine unrelated declarations, which is a
bigger and wronger answer than saying nothing.

Handshake test: `a_member_hover_is_resolved_through_the_objects_type` — it drives a real server, because `hover` is
written against `Session<DiskFiles>`.

## 3. Still open

```text
`std::string x = 123;` is not reported      the class-target conversion is conservative by design: `convertible_to`
                                            answers Unknown for a class, and `a_class_can_be_built_from` only speaks
                                            when it is *certain* the constructor cannot take the value. Making it
                                            catch this needs the constructors' arities, not a new check.
~~`auto y = x.back()` → `char&`~~            **already correct, and now tested.** `basic_string<char>::back()` answers
                                            `char&` and `std::vector<int>::front()` answers `int&` — the substitution
                                            was already in place (`types.rs::a_members_return_type_has_the_class_arguments_substituted`
                                            and `…_is_substituted_through_a_template_id_use`). This line was a guess when
                                            the document was written; the tests are the correction.
hover falls back to the name list            still true when the type genuinely cannot be computed (`decltype`, a
                                            template parameter, a macro-written initialiser) — and the fallback is
                                            outside this document's fix.
```

## The battery, and the one gap it found

`crates/cpp_code_analysis/tests/generics.rs` — four cases, three of which pass today:

```text
a_member_list_is_the_instantiated_classs          std::vector<int> offers `push_back`, and not a basic_string's members  ✓
an_alias_to_an_instantiation_offers_that_classs  `using string = basic_string<char>;` offers basic_string's members        ✓
a_nested_instantiation_has_its_members…           `std::vector<std::vector<int>>::front()` is `std::vector<int>&`          ✓
a_member_parameter_is_checked_after_instantiation  `std::vector<int>::push_back("text")` — **no diagnostic at all**          ✗ ignored
```

The fourth is the layer you named: **type checking after instantiation**. The argument is checked against nothing,
because the member's fact says `T value` and the check has no way to learn that the class was instantiated at `int`.

**Why it is one seam.** `completion` calls `members_of(index, …, class)` with `written.class_name()` — the spelling with
the template arguments **removed** (`completion/mod.rs:468`). So the member list is built from a class name that no
longer knows its arguments, and every consumer of it — completion, hover, `param-type-check`, `argument-count` — gets
facts written with `T`. The substitution machinery itself already exists and works: `sema::types::substituted_spelling`
is what makes `basic_string<char>::back()` answer `char&` and `std::vector<int>::front()` answer `int&`.

**The change to make**: pass the **argumented** spelling down (`std::vector<int>`, which `type_of_expression` now keeps
from the `auto` fix), have `members_of` substitute the class's parameter names with those arguments in each member's
`parameter_list`, `returns` and `type_of`, and the two checks inherit it without a line of their own. It is one function
and its callers, not a new feature — and `docs/overloads.md` §21 is the same mapping on the call side.

## The arguments now reach the member list — and the next seam is named

`SemanticModel::members_of` substitutes the class's arguments into every member before anyone reads it:

```text
std::vector<int>::push_back   (T value)  →  (int value)
std::vector<int>::front       T&         →  int&
```

The names come from the class's own declaration (`deduction::template_parameters_before`), the arguments from the
spelling `type_of_expression` now keeps, and the substitution is `sema::types::substituted_spelling` — the third user of
one function, not a new mechanism.

**It did not make the instantiated check pass**, and the measurement says why: the argument checks **skip member calls**
on purpose. `argument_count` gained a guard that ignores a callee which is a member access, because `_session.Run(…)` was
being judged against a free `Run()` and reported 19 times on a real project. So `v.push_back("text")` reaches no arity and
no type comparison at all — the substituted parameter list is never consulted.

**The next seam is therefore a different one**: resolve a member call to its member (`member_access_at` → the object's
type → `members_of` → the parameter list) and check **that**, instead of falling back to a free function of the same name
or to silence. That is the same resolution the hover now does, and doing it once — in a query both the hover and the two
checks call — is what makes the checks answer for members at all.

## The second seam, moved: member calls now reach the arity check

`argument_count` judged every member call as "skip", because `_session.Run(…)` was being compared against a free `Run()`
and reported 19 times on a real project. The replacement resolves the member instead:

```text
member_access_at(callee) → the object → its type (`std::vector<int>`) → members_of (substituted) → the member's
parameter list → any overload that accepts this many arguments answers it; otherwise the closest is reported
```

That is `judge_a_member_call` in `argument_count.rs`, and it is the same three steps the hover takes. Measured: it does
**not** make the instantiated test pass, and the reason is worth writing down — `push_back` takes exactly one argument and
the call passes one, so **the arity is right**. What is wrong is the **type**, and that is `param_type_check`'s question.

## The third seam, named precisely

`param_type_check` builds its `by_name` table from **this file's function definitions**
(`super::is_a_function_definition`). A member of a class is a **declaration** — `void push_back(T value);` — and the table
therefore never holds it, so a member call is skipped before any type is compared.

The change is the one just made for arity, one layer over: for a member call, take the member from `members_of` (whose
parameter spellings are already substituted — `(int value)`), strip the parameter name the way
`super::parameter_spellings_of` does for a definition, and compare the argument's type with **that** spelling. The helper
that strips a name from a written parameter already exists (`sema::overload::parameters_in` returns the spellings the
arity check uses); the check needs the name-free spelling from it, which is a smaller change than this document had
assumed.

## The third seam, closed — and a fourth one found on the way

`param_type_check` now judges a member call against the member:

```text
member_access_above(callee)   the access is the callee's *parent* (`push_back` is the identifier to the operator's
                              right), which is why the first attempt silently did nothing
  → the object's type          `std::vector<int>` (from the `auto`/`type_of` work)
  → members_of (substituted)   `(int value)`
  → parameters_in              `["int"]` — `overload::parameters_in` already strips the parameter's name
  → the argument's type        `const char*`, which `convertible_to` answers `No` for `int`
  → ParamTypeCheck             "this passes `"text"` where `int` is declared for `push_back`"
```

Two things measured on the way, both worth keeping:

- **`get_args`, not `children()`.** A string literal is a **token** in this grammar, so a walk over the argument list's
  child *nodes* is empty and the comparison never ran. `argument-count` had already learned this; the trace
  (`CPPLS_TRACE_MEMBERS=1`) is what showed the loop was empty rather than the types being wrong.
- **The free-function path never checked a named parameter.** `parameter_spellings_of` returns `int width`, and
  `parse_type_spelling` reads a type, so every real-world `void f(int n)` call went unchecked while an unnamed
  `void f(int)` was checked. The tests now pin the unnamed case; the named one is `#[ignore]`d with the reason, because
  fixing it makes this check fire on code it has never judged — a census first, per this project's rule.

Five tests now cover the seam: the template case reports, the fitting argument is silent, a plain class is checked, too
many arguments is `ArgumentCount`, and the free path is unchanged.