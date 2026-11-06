# Hover rendering — the contract, and what is still open

The hover card's code block is **generated**, never sliced out of the file. One module decides it,
`cpp_code_analysis::sema::render`, and the card, the overload list and the member list all print from it:

```text
render::an_item(fact, syntax, value) -> Vec<String>     the declaration, with its template head
render::a_line(fact, syntax, value)  -> String          one line, for a list
render::a_type(written)              -> String          a spelling through `Type`'s writer
render::parameters(written)          -> Vec<Parameter>  the recorded list, parsed and rebuilt
```

The rule for anything added here: **every character is a product of the model**. A type is a `Type` printed by its own
writer; a parameter list is parsed; a template head is built from the declaration's parameters; an annotation is
`ReflectionFact::said()`. The declaration's text is input at most (a spelling the fact recorded), never output.

## Done

* Functions and methods: modifiers (`static`, `inline`, `constexpr`, `virtual`, `explicit`), return type, name,
  parameters with types/names/defaults/packs, and the trailing clause (`const`, `&`, `&&`, `override`, `final`,
  `= delete`, `= default`, `requires`).
* Classes: the keyword its node kind carries (`class`/`struct`/`union`), bases, `enum`/`enum class`, the reflection
  annotation above it.
* Aliases (`using X = Y;`), variables (`static`/`constexpr`/`const`/`volatile`, a folded constant's value), enumerators
  (`Red = 1`), namespaces.
* Generics: template heads with kinds, defaults and packs; **members of a known specialization render instantiated**
  (`Box<int>::put` shows `const int&`, not `const T&`); a class use shows its own spelling as
  `As written here: \`Box<int>\``.
* The fact/parser repairs the rendering needed: `typename` stripped by the type reader; `members_of` retrying the
  qualified spelling; `qualified_from_the_index` taking the class declaration rather than a constructor;
  `finish_a_nested_name` following a head that is another class; an unresolved pointee refused instead of returned;
  `CppTemplateParam::{is_type_param,get_name}` searching the subtree.

## Open — in the order they block each other

### 1. `DeclFact::type_of` loses its shape match (the `shared_ptr` root cause)

A fact's type is **recovered** by matching a binding to a syntax shape by range at summary-build time. When that match
misses, the field becomes `None` silently. Measured with `inspect facts` on `bits/shared_ptr.h` and truncations of it:

| input | `element_type`'s `type_of` |
| --- | --- |
| lines 1..191 (stops right after the declaration) | present |
| lines 1..197 (the class's `#ifdef … #endif` block comes after it) | missing |
| the whole file | missing |
| lines 174..197 alone | present |
| lines 1..55 + the class | missing |
| lines 1..80 + the class | present |
| a fixture whose **last** token is an unterminated `/**` | missing |

So a field that belongs to a declaration is destroyed by text **after** it, which means it is not being read from the
declaration. The plan:

1. When the shape lookup misses, read the type from the binding's own `range` (it is the declaration's range) — a
   fallback in `declared_type_of_with` that cannot miss.
2. Count the misses (`inspect stats`) so the bug is visible instead of silent.
3. Then re-check `std::shared_ptr<Resource>::element_type` on `test_cpp`; the hand-written two-file fixture already
   passes (`the_class_after_the_operator("shared_ptr<Resource>") == Some("Resource")`).

### 2. The two readings

`FileIndexer::index` (the file's own text) and `index_rendering` (the cooked stream, ranges mapped back) both produce
summaries, and the same class can be described differently by them: `inspect facts` reads the first, a hover may use the
second. A fix verified in one can regress in the other — this is why `shared_ptr` has been "fixed" several times. The
open question is the design: one summary per file with both answers for the fields that differ, or a rule that every
query reads the reading that produced the fact it started from.

### 3. Gaps this pass found, each small and self-contained

* **`std::less<>` renders `std::less`.** `parse_type_spelling` drops an empty argument list, which changes what the name
  means. `Type::Named` needs to keep "had an empty list" (or the name keeps `<>`).
* **A hover on a template-id callee answers nothing.** `std::make_shared<Resource>(2)` — the callee is a template-id, not
  a plain name, and the name lookup at that offset finds neither. (Measured on `test_cpp/src/smart_pointers.cpp:50`.)
* **~~The prose around the card still prints raw fact strings~~ — done.** `kind_words` routes `type_of` and `returns`
  through `render::a_type`, so the sentence and the card below it print one type. Measured on
  `std::vector<std::string>& names();`: the card said `std::vector<std::string>&` while the prose said
  ``returning `std::vector<std::string> &` ``; now both say the former. (`tests/hover-latency`-style measurement:
  `inspect hover`, before and after.)
* **`the_type_the_macros_leave` is a fact-level repair still in hover.** It rebuilds a return type that a macro's name
  swallowed (`WINBASEAPI WINBOOL WINAPI SetConsoleOutputCP`). It should live where facts are built; hover should not
  own a text repair at all.
* **`const`/`volatile` restored only from specifiers.** A `Foo* const p` writes its `const` in the declarator; the card
  says `Foo* p`. Reading the declarator's own qualifier is the missing half (the fact drops both).
* **Operator hovers hit the keyword vocabulary popup.** On `operator bool`, the cursor is on `operator` and the
  keyword popup wins over the declaration. The card should be the declaration; the vocabulary is for a name that is
  not one.
* **A constructor's name resolves to the type.** Hovering `Derived` in `Derived() = default;` shows `struct Derived`
  rather than the constructor, because the name is both.
* **Bases are printed without their access** (`: Base`), which is right for a fact (it does not record the access) and
  wrong for a class whose base clause says `public`; the syntax has it.
* **Function-pointer parameter types** print as the model's unparsed name; `int (*cb)(int)` deserves `Type::Function`.
* **`value` in a lambda is coloured `method`** (`test_cpp/src/smart_pointers.cpp:64`). The use does not resolve to the
  lambda's parameter and falls through to the index, where a member function called `value` matches. Needs a
  reproducer in a fixture first.
* **Doc comments** are still the file's text, deliberately: they *are* documentation, and `render_documentation` formats
  them as blocks. If that changes, it is a separate decision from the code block.
* ~~**`diagnostics_stable`'s `a_finding_does_not_vanish_between_first_and_second_pull`** times out on a warm cache
  (120 s) and passes on a cold one.~~ **Fixed, and it was never a timeout.** The gate that withholds check findings
  was unsatisfiable after phase 0 of the indexing design, so the pull answered `[]` in milliseconds and the test waited
  for a finding that could not be published. See `docs/buglist.md` §28; it passes in 0.4 s.

## The first hover of a file, measured — and the write it used to take

A report from use: *"冷启动和热启动已经很快了, 但是 hover 有时候需要 loading 很长的时间"*. It is not the lazy refinement (that is
answerable from L1 and never blocks a hover) and not a lock held by the pump (`gate 0 ms` in every line below). It is
**the write lock a hover itself took**, and it was measurable in one run:

```text
$env:CPPLS_HOVER_ROOT = "E:\EmmyLuaCodeStyle"
cargo test --release -p cpp_ls --test hover_latency -- --ignored --nocapture
```

`crates/cpp_ls/tests/hover_latency.rs` opens the project, hovers positions in one file (round 1 is the first hover of
each position, round 2 the same ones after) and then a file at a time across the project, and prints the server's own
`a query took … permit … gate … session … ran …` lines beside the numbers. Before the change:

```text
per-file first hover     40–405 ms, 135 ms average
  the breakdown          `a request queued the modules` held the analysis for 210–305 ms  ← a WRITE
                         a query took 70–101 ms — permit 0, gate 0, session 0, ran 70–101
```

**`ran` is the hover's own work; `session` 210–305 ms is a write the hover took.** `Session::catch_up` read **every
unread include** of the file inside the request, and each include is a parse that can be a timeline walk: for a `.cpp`
whose closure is a hundred standard headers, that is a third of a second — spent under the write lock, so every other
request in flight waited for it too.

**The change** (`Session::catch_up`): the file itself is still read inline (that is what the answer needs), and its
unread includes are read **eight at a time**; the rest are **queued for the pump**, which the caller wakes. Files
already queued are skipped so the two never parse the same header, and `needs_catching_up` stays true while any include
is unread, so the next request about the file reads the next eight.

```text
                                  before            after
per-file first hover              135 ms average    53 ms average  (worst 405 → 181 ms)
the write a hover takes           210–305 ms        gone — no `session` line above 0 ms
one file, first hover             190 ms            85 ms, then 0.2–3 ms for every later position
```

**What is left, and where to look next.** `ran` is now 65–187 ms on a *cold* file and 0.2–3 ms once it is read. That is
the query's own work: `view_of_the_file` parses the file's text, and `definitions`/`type_at` search an index that is
still filling. The probe prints the breakdown per file, so the next step is `ran` on the slowest few rather than a
guess — the shapes it names are a big file (a parse) and a name the index has not resolved yet (a search).
