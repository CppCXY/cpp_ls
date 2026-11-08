# The bug list

Two kinds of entry: **a wrong answer**, which is a fact about the analysis, and **a measured cost**, which is a fact about
the machine. Each entry says how it was measured, so the next reader can reproduce it rather than trust it.

## Open

### 4. The macro environment a written view wants is never built by the pump

* **What is missing.** A file's **written** view (`view_of_the_file`) is parsed with the macro bodies its includes
  define, so `_STD_BEGIN struct widget { … }; _STD_END` can place `widget` in `std`. The value is `the_macros_of` — a
  103.7 ms walk over the closure — and it is meant to be built by the pump's drain into `Drain::environments`. **Nothing
  fills that list.** The loop that did read a queue (`macro_work`) the loop above it had already drained, so it was dead
  code; the list has been always empty.
* **Evidence, and why it is not wired naively.** Feeding it from the ask channel (`Ask::Cook`) was tried and measured on
  `CodeFormatCLib`… `LSPHandle.cpp` of `E:\EmmyLuaCodeStyle`:

  ```text
  the project read        6.5 s  -> 26.5 s
  a diagnostic pull        15 us -> 11.7-13.4 s
  the whole probe           7 s  ->    87 s
  ```

  One environment build per ask, and a `view` asks on **every** miss. Reverted (`29f8d8a`), with the numbers written in
  the code where the slice belongs.
* **What it needs.** A membership set (the ask is per file, not per view call), a bound on what one drain builds, and a
  rule for when an ask is *answered* — `known_macros_of` is the test, and it must be consulted before the ask, not
  after. Then the same measurement must show the project read unchanged.

### 1. `auto x = std::move(e)` has no type, so every member of `x` is unresolvable

* **Where.** `CodeFormatCLib/src/CodeFormatCLib.cpp` of `E:\EmmyLuaCodeStyle`:
  `auto diagnostic_results = std::move(result.Data);` then `for (auto &diagnostic : diagnostic_results)` — the editor says
  `diagnostic`'s type is not known, so `diagnostic.Message` resolves nowhere.
* **Measured** (`tests/codestyle_probe.rs`, `--ignored --nocapture`):

  ```text
  type_at(`Data` of `result.Data`)   std::vector<LuaDiagnosticInfo>     ← the member really is typed
  std::move candidates               move(_Tp&& __t) → typename std::remove_reference<_Tp>::type &&
                                     move(_II, _II, _OI) → _OI          ← the algorithm overload
  type_at(`std::move(result.Data)`)  Unknown(Ambiguous("std::move"))
  type_at(`diagnostic_results`)      _Tp&&
  ```

* **What is not the cause**, measured rather than assumed: the member substitution (it answers the vector), the arity of
  the candidates (the algorithm overload is refused by three-parameter arity), and `the_return_the_overloads_agree_on`
  (with `CPPLS_TRACE_UNITS` on it is **never called** for this expression).
* **Where it is.** `what_a_call_has` (`index::project`, from about line 6300) already deduces —
  `deduced = deduce_the_parameters(&names, written_list.as_deref(), argument_types)` — so one of `names`,
  `written_list` or `argument_types` is empty for this call. The next step is one print of those four values at that
  site, then the fix follows from which one it is.

### 2. Editing a file freezes the file — and the library is *not* where the time goes

* **Reported from use.** "只要我编辑代码，当前文件就会有较长时间的卡死".
* **What a keystroke costs at the library level, measured** (`--release`, `E:\EmmyLuaCodeStyle`,
  `CodeFormatCLib/src/CodeFormat.cpp`, in the setup that resolves the project — the one `tests/codestyle_probe.rs` uses,
  where `members_of(LuaParser)` answers 71 members):

  ```text
  the project read                    1.70 s
  the file's rendering                0.01 MB, 1 422 token(s)                ← the file's own text, no closure spliced
  the first view                      1.7 ms
  a body keystroke, 5x   the pump     7.2 / 8.6 / 6.8 / 9.0 / 7.5 ms          read as the program, every time
                         catch_up     7–10 µs
                         a fresh view 1.4 / 1.4 / 1.5 / 1.4 / 1.9 ms
  ```

* **Three assumptions this entry used to carry are now false, and the measurements say why:**
  1. *"An edit reparses the whole expansion."* There is no expansion to reparse for an `#include` file: its rendering is
     its own text (1 422 tokens), and cross-file answers come from the **project index**, not from a spliced stream. The
     5 MB stitched stream this project has measured before is the **`import std;` program read** — the modules path,
     which is real work but is not what typing in `CodeFormat.cpp` does.
  2. *"A keystroke costs the whole re-read."* A body keystroke costs **~9 ms** in the pump and 1.4 ms for the view, and
     the view cache absorbs the repeats. (`tests/keystroke_probe.rs`: 10 body keystrokes, 7/8/9 ms, one file walked.)
  3. *"The first read is the closure."* `didOpen` + one pump step is 179 ms on the file probed there, and the fact count
     quoted for it (192 534) is the **project index's** total, not that file's closure — the number was read off
     `cache_sizes()` and mis-attributed here. It is now corrected rather than left standing.
* **What is still true, and is worth fixing:** a **directive** keystroke re-walks every file of the unit
  (11 files, **165–168 ms**, 20× a body keystroke), and the `import std;` read is seconds. Those are the costs the
  three-artifact split addresses.
* **What is therefore unexplained.** A freeze that the library does not charge for is not in the library's re-read. It
  is on the **server** side — `crates/cpp_ls`'s `on_did_change` and whatever the client fires next (pull diagnostics,
  whose handler takes a write hold; semantic tokens; inlay hints), or the client itself. The next measurement is a
  running server against this project with the handler's own timings, not another library probe: every library probe
  now agrees that a keystroke is single-digit milliseconds.
* **Progress on the split.** The parser now parses a **token range**, not only a whole text
  (`CppParser::parse_tokens`, step 1 of [`parse-architecture.md`](parse-architecture.md), four properties pinned in
  `tests/token_range.rs`). It is the enabling piece for the two costs above — the `import std;` read and the directive
  re-walk — and it is **not** offered as the freeze's fix.

### 3. Opening a file costs the whole closure before any query answers

* **Reported from use.** "我新打开的文件会导致CPU轰鸣，当前页面需要长时间的加载，导致其余LSP功能几乎无法响应".
* **Measured.** The open itself is small: `didOpen` + one pump step is **179 ms**, and the file's first view **1.7 ms**.
  What roars is around it: reading the project takes **7.0 s** on this checkout (`tests/codestyle_probe.rs`,
  `what_a_keystroke_costs_on_the_real_closure`: one `index_everything` over the CodeStyle project), and an
  `import std;` program read is seconds more. Which of those the reader experiences as "CPU roaring while the page
  loads" is not settled by a library probe — the handler's own order is what decides it, and that is #2's open question.
  The same probe on the file the report is about (`LSPHandle.cpp`): rendering 2 181 tokens, first view 1.9 ms, and a
  body keystroke costs the pump 15.6–18.8 ms — nothing at the library level accounts for a freeze.

## Closed

### A body edit rebuilt the macro environment — 103.7 ms of closure walk per keystroke

* **Where.** `Session::buffer_changed` → the cache `the_macros_of` fills. A file's `MacroEnvironment` is built by walking
  its **closure** and copying every include's `#define` bodies; the drain's own log measures **103.7 ms on MSVC's
  headers**, and the build runs under the **read** lock, which a writer waits for.
* **The defect.** The environment was cached against the file's **content hash**, so a single character typed in a
  function body produced a new key, a miss, and a rebuild — for macros that had not moved. Every request that needed its
  one write step (`prepare`, `catch_up`, a diagnostic pull) then queued behind ~100 ms of work the edit had not made
  necessary. This is the shape behind "hover has a loading delay every time" and "diagnostics do not update while I
  type": not one slow query, but a hundred milliseconds of background work re-done per keystroke, under a lock every
  request needs.
* **The fix.** The session already knows the difference — `directives_moved` answers `environment` by hashing the
  file's directive lines, and it is what decides whether a file's *dependents* need re-reading. When that answer is
  "nothing moved", the environment is **re-keyed to the new content** instead of being thrown away
  (`Session::buffer_changed`), and a directive edit still misses, which is the honest state until the closure is walked
  again. One environment per file, not one per keystroke.
* **Pinned by** `session::tests::a_body_edit_keeps_the_environment_its_includes_built`: the same `Arc` before and after
  a body edit (`Arc::ptr_eq`), `None` immediately after a directive edit. Verified to fail without the fix.

### An object's own parameter list was read as a function's

* `LuaParser p(file, std::move(luaLexer.GetTokens()));` — `p.HasError()` answered "`p` is a `LuaParser(file, std::move)`,
  the class is out of the index". The type reader wrapped the object's constructor call into `Type::Function`; the fact
  builder stripped it afterwards and the expression engine did not.
* Fixed in `7a1993a`: the reader is told whether the declaration declares an object, and an object's own list is its
  constructor call. Measured after: `type_at(p) = "LuaParser"`, `members_of(LuaParser)` = 71 members including
  `HasError`.
