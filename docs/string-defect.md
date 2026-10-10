# The `std::string::` defect, traced to a reproducer

`std::string::` offers **0 items**. This is the trace, the measurements, and the one step that is left. Written
because four plausible causes were wrong in a row and the fifth was found by printing facts rather than by reading.

Reproduce with `examples/completion_latency.rs` (the `--- the class-hierarchy cursors ---` section) and
`CPPLS_TRACE_MEMBERS=1`, on MSVC 14.29's standard library.

---

## 1. What the cursor asked, and what it got

```text
names_in_a_scope(std::string): in the tree false, names_a_type false,
    definition Unknown(NotDeclaredHere("std::string")) | facts named "string": []
```

`names_in_a_scope` reaches `std::string` by the path a **qualified name that is not a class in the buffer** takes:
no alias in the local tree, so it asks the index whether the spelling names a type, and the index says no.

## 2. Four measurements that narrowed it

```text
  index: 163 summaries, 16303 distinct names
  by_name:  "string" 0   "basic_string" 0   "vector" 14
  by_scope: "std::basic_string" 0
  files:    xstring 1, and seven others whose names contain `string`
```

`<xstring>` **is** in the index — so this is not a missing header, not a closure that stopped early, and not a
stale cache.

```text
  <xstring> has 376 declaration(s); the 1 named *basic*: ["basic_string_view" kind Type scope Some("std")]
```

**`basic_string_view` is recorded and `basic_string` is not — from the same file, the same namespace, six hundred
lines apart.** That single line is what turned a search into a defect.

## 3. What is *not* the cause (each checked, each wrong)

```text
  "the default template argument breaks the parameter list"
      Disproved: `template <class _Elem, class _Traits = char_traits<_Elem>,
      class _Alloc = allocator<_Elem>> class basic_string { void f(); };` records
      `basic_string` **and** all three parameters. See `examples/template_default_probe.rs`.
  "the class is behind a #if"
      Disproved: the last directive before `basic_string` is at `xstring:2285`
      (`#ifdef __cpp_lib_constexpr_string` / `#endif`), and the class is at 2344.
  "the macro-written namespace loses the class"
      Disproved: `basic_string_view` comes out with `scope Some("std")` from the same file.
  "the index was never given the file"
      Disproved: 376 declarations are held for it.
```

## 4. Where it stands

`xstring:2344` writes the class once and nowhere else (`class basic_string {` — no other declaration on that
name in the file), so this is not the forward-declaration/definition ambiguity that `bases_of` had to learn about.
The declaration is in the file, the file is in the index, the index holds 376 of its declarations **including the
three template parameters written on the line directly above** — and the class name itself is not among them.

So the loss is in the **fact builder's handling of this one declaration**, not in any lookup. Two candidates remain
and the next step decides between them:

```text
  a)  the scope walk records `_Elem`, `_Traits` and `_Alloc` (they are in the summary) and then
      fails on the class that follows them — a template-head/declaration seam in `declarations.rs`
  b)  a declaration count or depth bound inside `build_facts` stops before it, and the three
      parameters are simply the last things recorded before the stop
```

The instrument that settles it is already in the tree: `examples/dump_tree.rs` prints what the grammar made of a
text, nodes **and** tokens. Feed it `xstring:2343-2345` and the question becomes "is there a `ClassDecl` node at
all", which separates (a) from (b) in one run.

## 5. Why this is worth more than the one cursor

`std::string` is a **`using` alias** — `xstring:4871`, `using string = basic_string<char, char_traits<char>,
allocator<char>>;` — so a reader writing `std::string::` is asking two questions in a row: *which class is this
alias*, then *what does that class have*. The alias half needs the class to be in the index, and the class is not
there. Every other spelling that resolves through `basic_string` is affected the same way, which makes this the
largest single missing answer in the standard library rather than one cursor's problem.

The sibling defect is already fixed and is the reason `std::ios::` works: `bases_of` looks for a class's
declarations **by its bare name as well as by its qualified one**, because a raw reading of a header that opens its
namespace with `_STD_BEGIN` files the class at file scope while the cooked reading of the same file files it in
`std`. Measured: `std::ios::` went from 0 to 32 items, `std::ostream::` 14 → 46, `std::istream::` 9 → 41.
