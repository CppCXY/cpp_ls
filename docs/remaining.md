
## A macro's inlay hints — the one row not done

```text
macros.c:  #define SUM(count, ...) (impl)(count __VA_OPT__(, ) __VA_ARGS__)
           int n = SUM(selected, 1, 2, 3);        // no `count:` hint over `selected`
```

Inlay hints resolve a call's parameter names from the **callee's declaration** (`inlay::parameter_hints` →
`callee_of_a_call` → `parameter_names_for`), and a macro has no declaration, so a macro call produces nothing.

Where the names are, and why this is a small job rather than a large one:

* `Session::macro_definition_text(path, offset, name)` (added this round) returns the directive's **text**, which
  contains the parameter list — `SUM(count, ...)` — joined across continuations;
* the call sites are in the **written** text, which `FileView::the_written_text` now returns for either kind of view.

The missing piece is the *placement* loop: it needs the calls found in the written text (a text scan for `NAME(` with a
matching macro, guarded against string literals and comments) and the arguments split at top-level commas. That is a
feature rather than a fix, and it is the one thing on the reported list this round did not do.

The other four rows — hover on a use, hover on a definition, hover on a predefined macro, go to definition — are done,
pinned by `the_c_macro_questions_the_editor_asks_are_answered` in `crates/cpp_ls/tests/handshake.rs`.
