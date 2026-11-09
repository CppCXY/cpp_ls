# The bug list

Two kinds of entry: **a wrong answer**, which is a fact about the analysis, and **a measured cost**, which is a fact about
the machine. Each entry says how it was measured, so the next reader can reproduce it rather than trust it.

## Open

### 6. One keystroke of one file costs a whole-program read, and the pump never goes idle

* **Reported from use.** "CPU 风扇转的程度…后台的计算量依旧巨大…几百行的文件不应该有如此之大的计算量".
* **Two of the three claims below are now corrected by measurement**, and the correction matters more than the entry's
  first draft:
  1. **The queue does reach idle.** The `61 pending / 61 cooking` was an artifact of asking the *library* to drain a
     queue whose cooking is claimed by the **server** (`handlers::initialized` claims 60-odd, renders and commits under
     a read lock). Simulating the server's loop in the probe (`what_keeps_the_pump_busy`, `pump round`):

     ```text
     pump round  0: claimed 61, built 61 | pending_work  66 | cooking  66
     pump round  1: claimed 64, built 64 | pending_work   2 | cooking   2
     pump round  2: claimed  2, built  2 | pending_work   0 | cooking   0   ← idle
     ```

     So "the pump never idles" is **not** established, and the fan is not explained by a queue that cannot empty.
  2. **The 3.6 s is a *first-time* timeline build, not a per-keystroke re-acquisition.** The unit is built the first
     time the edited file is read after its closure is in hand; the next keystrokes are 8–18 ms (measured with five in a
     row). Pinning the open file's unit against eviction changed the number **not at all** (3.73 s → 3.60 s, noise) and
     was withdrawn.
* **The 1.15 s completion, broken down** (`what_keeps_the_pump_busy`, same cursor, same idle session):

  ```text
  name_completions     1.145 s   (24 name(s))     ← the cost
  member_completions   1.0 ms     (UnparsableName — nothing to list at `LSPHandle::` for this probe's cursor)
  completions          10.3 ms    (1 item)         ← the second call, warm
  ```

  So the query is **cold-state work in `name_completions`**, and the warm call is 10 ms. The path is
  `global_offers` → `global_names` → `declarations_in_scope(None)` →
  `ProjectIndex::visible_declarations_where`, which **enumerates every declaration of the whole closure** and applies
  the prefix filter per fact (a 280-line file whose closure is ~1 600 headers). Its budget
  (`MAX_COLLECTED_NAMES`) caps the *result*, not the enumeration.
* **The fix, therefore**: a prefix is a *query*, not a filter — the index already holds names by spelling, and an
  unqualified completion should ask "names starting with `w`" rather than walk the closure and throw away everything
  else. That is the next commit, and the probe above is its before-number.
* **Measured** (`tests/codestyle_probe.rs`, `what_keeps_the_pump_busy`, on `E:\EmmyLuaCodeStyle`, one project file
  added — `LSPHandle.cpp`, 280 lines):

  ```text
  one keystroke: advance(1)         3.70 s        ← ONE step, one file
  a completion                      1.19 s, 1 item(s)
  until idle after the edit         400 slices, still pending 61 | cooking 61
  store stats                       reused 1544, rebuilt 48        ← of the project's 1590 files
  ```

* **Three separate facts, and they compound:**
  1. **The whole program is read per keystroke.** One `advance` step after an edit costs 3.70 s, which is the
     whole-program read (walk + cook every file of the closure + insert) — the same "one indivisible piece" the code
     documents at 5.09 s. A 280-line file should cost its own parse.
  2. **The queue never empties.** After draining 400 slices, `pending_work` sits at 61–62 and `pending_cooking` stays
     **61 for ever**: the idle drain (where cooking is marked and `want_the_closure_cooked` runs) is never reached, so
     61 files keep asking to be cooked and the pump spins — that is the fan.
  3. **The requests pay it.** `advance` needs `&mut Session`, so the server runs it under the write lock: a completion
     arriving in that window waits, which is why it took 1.19 s and answered one item.
* **What to change, in order** — none of it is a lock:
  1. **The program read must not be triggered by a `#include` file's edit.** It exists for module interface units
     (`import std;`); for an `#include` project the per-file summaries already answer. Gate it on "this file imports a
     module" (or on a request explicitly asking for the program), not on "a pause in typing".
  2. **The queue must reach idle.** 61 pending after 1 600 steps means the frontier never closes: every file indexed
     re-adds its includes, and `Work::add` refuses only files already *worked*, while a file whose summary came from the
     disk counts as worked — so the two disagree about a file's state. Fix the disagreement, then the idle drain runs.
  3. Only then: the cooking backlog (61) drains, and the requests stop waiting for a writer.

* **Where the 3.7 s is, found** (`CPPLS_TRACE_UNITS=1`, per-part timings inside `index_one`): the edited file's own
  work is **12 ms** — load 1 µs, read 12 ms, 17 includes queued in 20 µs — and the remaining **3.6 s** is
  `the_timeline_to_read_through` → `unit_and_state_of` → `translation_unit_of`, which finds no timeline in memory and
  **decodes one from the disk cache (or walks it)**: a whole-program rendering, megabytes, once per keystroke.

  ```text
  with that arm skipped       one advance step after an edit:  8.75 ms
  with it (as designed)       one advance step after an edit:  3.73 s
  ```

* **And it cannot simply be skipped.** Two contract tests fail when it is:
  `a_file_opened_after_the_index_was_built_is_read_through_a_walked_timeline` and
  `a_keystroke_in_a_body_reads_the_file_through_its_timeline`. Reading the reader's file through the program's timeline
  is deliberate: it is what makes the second pass have nothing to repair. The defect is that the **bounded units table
  evicts the open file's timeline during the project read**, so the contract is paid for by re-acquiring the whole
  program on every keystroke.
* **The fix, therefore**: keep the **open file's timeline hot** — pinned against the units table's eviction — so the
  contract holds at milliseconds, and the acquisition happens once per open, not once per keystroke. That is the next
  commit; the per-part instrumentation (`[step]`, `[advance]`) stays behind `CPPLS_TRACE_UNITS`.

* **The 1.15 s completion, localized** (`CPPLS_TRACE_COMPLETION=1` now prints `visible_names`' four segments):

  ```text
  the position: scope "", written "Refr"
    the class tier: 1.1678 s      ← the whole cost
    the chain:          1.6 ms
    the global tier:    3.6 ms
    the sort:            16 us
    the listing:        1.17 s for 24 name(s)
  (the same request, warm: class 70 us → 4.3 ms in total)
  ```

  It is `the_class_a_definition_belongs_to` + `names_of_a_class` → `ProjectIndex::members_of` → `members_of_inner` →
  **`resolve_aliases_or_unresolved`**, whose resolution of the class name is the index's declaration query — and its
  first call is the guard-evaluating visibility walk (1.2 s on this closure). Warm it is 70 µs.

  **Three earlier rounds cleared the wrong suspect because of a probe artefact**: they timed
  `Session::members_of("LSPHandle")` *before* asking for the completion, which warmed the answer to 16–25 µs. The
  number that matters is the cold one, and it is the first request after each edit — `definition_answers` is cleared on
  every index change and the member list is dropped when any file it was made of changes (and `LSPHandle`'s list
  includes the out-of-line constructor's fact, which lives in the `.cpp` being edited).

* **The next change, named**: give the *class* resolution in `resolve_aliases_or_unresolved` the cheap visibility
  (`include_closure` + the `names` postings) instead of the walk, falling back to the walk when it finds nothing. It is
  a different question from the completion's *name list*, which is why the cheap version is allowed here and was not
  there: a class's members are its own facts, and a class behind conditionally-included code still has the members a
  reader would type. It must be checked against
  `a_name_only_the_cooked_reading_declares_is_offered_over_the_wire` and
  `a_c_reader_gets_completion_and_signature_help_for_what_the_file_defines`, which are the two contracts the *name-list*
  version broke.

* **What this goal fixed, and what it did not**: `LSPHandle` resolving to its constructor (§5, `38e58cf`) is fixed and
  measured; "the pump never idles" is disproved; "a keystroke triggers a whole-program read" is corrected to "the
  *first* read of a file builds its timeline once" (3.6 s, under the write lock); the completion's 1.15 s is localized
  to one function and **not yet fixed**.

### 5. A class can vanish from the index when its own source declares a member of the same name

* **Reported from use, twice.** "你把 `LSPHandle` 识别为了构造函数本身而不是类本身" — completion after `LSPHandle::`
  stops popping, hover has no type, diagnostics in that file are wrong.
* **Measured** (`tests/codestyle_probe.rs`, `what_the_analysis_says_about_lsp_handle`, on `E:\EmmyLuaCodeStyle`):

  ```text
  definition(LSPHandle) = Unknown(Ambiguous("LSPHandle"))
    candidate: Function scope Some("LSPHandle") in LSPHandle.cpp
    candidate: Function scope Some("LSPHandle") in lsphandle.h      ← the header's constructor
  -- and yet the index holds the class:
    Type scope None in lsphandle.h at 300                           ← every_fact_named finds it
  ```

  So the class is **in the index and not among the candidates**: `certain_declarations`' "a declaration this file
  writes wins over one it includes" keeps local facts and same-entity header facts, and `LSPHandle.cpp` writes
  `LSPHandle::LSPHandle(int)` — a member *in the class's scope* whose name is the class's. The class is at file scope,
  the constructor at `LSPHandle`'s, and the rule compares files without comparing scopes.
* **And it is intermittent.** The same probe, same build, a later run answered `Yes(Type … lsphandle.h at 300)`. What
  differs between the two runs is not established — the project's `.cppls` cache was cold on the first and warm after
  it, which is the first thing to test (remove the cache, run the probe, repeat with it warm).
* **What is *not* verified, and must not be claimed.** A candidate fix (keep a candidate whose `scope` differs from
  every local candidate's) makes the probe answer `Yes(Type)`, but **removing it also leaves the probe answering
  `Yes(Type)`**, and no fixture built so far fails without it. It is therefore not in the tree. The guard that is in the
  tree (`index::project::tests::a_member_named_like_its_class_does_not_hide_the_class`) records the contract and passes
  today.
* **Next measurement, in order**: (1) cold cache vs warm, twice each, to see whether the *reading served from disk* is
  what loses the class fact; (2) if cold reproduces, compare the two summaries' facts for `LSPHandle`; (3) only then
  decide between "the candidate filter is wrong" and "the stored reading is wrong".

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

### 9. `nlohmann::json::object()` has no type — the alias was a no-op, and the member's fact has no scope

* **Reported from use** (picture 3, the half that is not the hover): hovering the variable `object` no longer shows a
  function, but its type is unknown — "the type of `nlohmann::json::object` is not known here" — and go-to-definition
  on `object` finds nothing in `json.hpp`.
* **Measured.** `definition("nlohmann::json::object")` is `NotDeclaredHere` while `members_of("nlohmann::json")`
  answers **53 members**; the alias itself resolves (`define` on `json` → `json_fwd.hpp:62 type in nlohmann type
  basic_json<>`), and the fact exists: `facts --file json.hpp --name object` lists
  `1960:23 object function returns basic_json parameters (initializer_list_t init = {})`.
* **Fixed, half of it: the alias rewrite was a no-op.** `through_an_alias`'s first arm answers the entity's own
  qualified name, and for an ordinary **type alias** that name *is* the prefix — `using json = basic_json<>;` in
  `namespace nlohmann` gives `nlohmann::json` back — so the rewrite returned the spelling it was given, the search
  found nothing again, and the type-alias arm below (which reads `type_of`, `basic_json<>`, and names
  `nlohmann::basic_json`) was never reached. Guarded with `qualified != prefix`; traced with `CPPLS_TRACE_ALIAS=1`:

  ```text
  [alias] trying nlohmann::json::object
  [alias] prefix nlohmann::json: ["json Type scope Some("nlohmann") type Some("basic_json<>")"]
  [alias] trying nlohmann::basic_json::object      ← the rewrite now happens
  ```
* **Open, and it is the bigger bug: the member's fact has no scope.** The retry asks
  `nlohmann::basic_json::object` and still finds nothing, because the fact at `json.hpp:1960` is stored with
  `scope: None` (`facts --name object` prints it with no "in …"). The **last** member fact that has the class's scope
  is `assert_invariant` at `1253`; everything after it — `set_parents` at `1274`, the static `object` at `1960`, the
  hundreds of members in between — is filed as file-scope declarations. `members_of("nlohmann::json")` therefore
  answers **53** members for a class that declares hundreds.
* **What is established about where it is *not*.** The tree is right: `inspect tree` at `set_parents` shows
  `Syntax(ClassBody)` → `Syntax(ClassDef)` → `Syntax(NamespaceDecl)` — so the **parser keeps the class body** and the
  **symbol/scope layer** is what loses it. Three fixtures built from the exact source around 1253 (the `#if` inside a
  body, the lambda, the macro `try`/`catch`, the `switch` of blocks) all keep the scope, so the trigger is earlier in
  the twenty-three-thousand-line header.
* **Next**: bisect `json.hpp` — cut ranges above 1253 and re-ask the scope of `object` — or diff the **rendering**
  (`CPPLS_DUMP_RENDERING_DIR`) against the source at 1253..1274, since the scope tree is built from the rendering and a
  macro that expands to a brace is the first thing to look for there.

### 10. `import std;`: `vector` pointed at a forward declaration, and `map`/`cout` are not found at all

* **Reported from use** on `C:\Users\zc\Desktop\test_module`, `src/std_module_demo.cpp` (an `import std;` file with no
  includes): `std::vector`'s card described a class with no body, "declared in `format:3090:36`"; `std::map` and
  `std::cout` had **no hover and no completion**.
* **`vector` — fixed, two layers of it.**
  1. The index holds three type facts named `std::vector`: a **forward declaration** in `<format>`
     (`template <typename, typename> class vector;`), the **definition** in `stl_vector.h`, and the
     `vector<bool>` **specialization** in `stl_bvector.h`. The hover's Ambiguous arm took the **first** fact whose kind
     is `Type` — the forward declaration. It now takes the **definition** first and, among definitions, the one with
     the **most template parameters** (the general declaration; a partial specialization is a narrower shape).
  2. `declaration_markdown` then **re-asked the index** for the chosen fact (the re-ask exists to recover a *binding*'s
     missing type/returns/parameters) — a class fact has none of those either, so the card was replaced by whatever the
     bare spelling answered: traced with `CPPLS_TRACE_HOVER=1`, `[card] heard vector … => indexed stl_bvector.h`.
     A `Type` or a `Namespace` is no longer re-asked.
  Measured after: `` template <typename _Tp, typename _Alloc = …> class vector : _Vector_base<_Tp, _Alloc> `` with
  "As written here: `std::vector<std::string>`".
* **And the tool had to be told about modules to see any of this.** `inspect` never read the module interface units, so
  every `std::` hover in that file was `none` while the editor showed a card. It now calls
  `read_the_modules_a_file_imports` the way `handlers::read_the_modules` does on `didOpen`.
* **`map` and `cout` — open, and narrowed to two questions.** The new probe
  `crates/cpp_code_analysis/tests/std_set_probe.rs` (ignored) says, in one session:

  ```text
  read 1 interface unit(s), `std` -> .../include/c++/bits/std.cc
  closure of `std`: 384 file(s) — contains stl_map.h, iostream, stl_vector.h, format
  visible files from the demo: 527 — contains the same, each with a summary, same spelling
  definition std::vector: Yes .../stl_vector.h      definition std::string: Yes .../stringfwd.h
  definition std::map:    NotDeclaredHere           definition std::cout: NotDeclaredHere
  definitions map:        1 declaration(s)          ← the BARE name finds it
  definitions std::map:   NotDeclaredHere           ← the QUALIFIED name does not
  members_of std::map:    NotDeclaredHere("map")
  ```

  So the fact is in the index, its file is visible, its spelling matches, and the **bare** name finds it — the
  **qualified** match is what fails, exactly as `files_declaring`'s `matches(fact, name)` compares the fact's
  `qualified_name()` with the written one.
  1. **`map`**: print `fact.qualified_name()` for the one candidate `definitions("map")` answers. The suspicion is a
     version namespace — libstdc++ wraps declarations in `_GLIBCXX_BEGIN_NAMESPACE_VERSION`, and a fact whose scope is
     `std::__cxx11` matches `map` by its last segment but not `std::map` by its full spelling. The fix then belongs in
     `matches`: a qualified spelling should match a fact in an **inline** namespace of that scope.
  2. **`cout`**: it fails even **bare**, and its file (`iostream`) is reached **only** through the module, so the two
     filters that can drop such a fact are `only_what_is_exported` (`through_an_import`) and the conditional guard of
     `extern ostream cout;`. One print of both for that one candidate decides it.
## Closed

### 7. `hover` answers with an index fact while `define` answers with the file's own declaration

* **Reported from use** on `C:\Users\zc\Desktop\github\EmmyLuaCodeStyle`, two pictures that look unrelated and are
  the same defect:
  * `LSPHandle.cpp:139`, `auto result = std::make_shared<lsp::DocumentFormattingResult>();` — hovering `result` shows
    `` `codecvt_base::result` — a type of type `int`, in `codecvt_base`, declared in `xlocale:581:11` ``, with the
    "inside a conditional block" caveat.
  * `LSP.cpp:12`, `auto object = nlohmann::json::object();` — hovering the **variable** `object` shows the **static
    function** `basic_json::object(initializer_list_t init = ...)`.
* **Measured, and the two answers disagree about the same cursor** (`tools/inspect`, same project, same file):

  ```text
  inspect hover --project <p> --file CodeFormatServer/src/LSP/LSPHandle.cpp \
      --needle 'auto result = std::make_shared<lsp::DocumentFormattingResult>' --shift 6 --index
      hover at 139:11: `codecvt_base::result` — a type of type `int`, in `codecvt_base`
  inspect define ... same offset --index
      picked   LSPHandle.cpp:139:10   result   variable
      found    LSPHandle.cpp:139:10   result   variable
  inspect hover ... --cook          (no --index)
      hover at 139:11: `result` — a variable of type `shared_ptr<lsp::DocumentFormattingResult>`
  ```

  So the **full index is what flips the answer**: with it, `hover` resolves the name to a fact of the same spelling
  from the standard library; without it, to the local. `define` is right in both runs.
* **The card's content is not wrong** — MSVC's `<xlocale>:581` really is `using result = int;` inside
  `class codecvt_base`, and that line really is inside a conditional region. The defect is *which* `result` the hover
  was asked about: a declared variable was answered with an unrelated type of the same name that happens to be in the
  closure. `LSP.cpp:12`'s `object` is the same shape (a variable answered by a function).
* **What is established about where it is not:** the fact for the local is right (`facts --name result` lists
  `139:10 result variable type auto`), `define` picks it, `type` answers it
  (`` `result` is `shared_ptr<lsp::DocumentFormattingResult>` ``), and the offset is the same in all three commands.
  The two commands differ only in the **function they call first**: `hover` goes through
  `hover_text` -> `hover_of_the_program`, `define` calls `session.definition` directly; and `hover` also asks
  `session.header_at` before that.
* **Found, and fixed — it was the card, not the lookup.** The probe (`CPPLS_TRACE_HOVER=1`) printed what the two
  queries answer and then what the card heard:

  ```text
  [hover] at 5546: picked Yes(ProjectDefinition { file: LSPHandle.cpp, fact: { name: "result", local: true } })
  [card]  heard result local true type_of None => indexed .../xlocale:result      ← the card re-asked the index
  ```

  `declaration_markdown` re-asks the index for a declaration whose fact has no type, no return type and no parameter
  list — which is **every fact built from a binding**, and a hover on a declaration's own name gets one. A binding
  carries no type (deliberately: the type is a fact about the file's text), so the condition was always true for a
  local, and the lookup was a **bare name**, because a local's `qualified_name()` is just its spelling — a function
  body contributes no scope segment. `index.definition("result", LSPHandle.cpp)` answered the standard library's
  `codecvt_base::result`; `LSP.cpp`'s `object` was answered by `basic_json::json_value::object`.
* **The fix**: a local is never looked up by name in the index (`if !found.fact.local && …`). The card then keeps the
  local's own fact, and the deduced-type note it already had fills the type in:
  `` `result` — a variable of type `shared_ptr<lsp::DocumentFormattingResult>`, declared in `LSPHandle.cpp:139:10` ``.
* **Pinned by** `reported_hover::a_local_is_not_answered_by_a_same_named_declaration_from_the_index`, a fixture whose
  header declares `using result = int;` in another namespace and whose `.cpp` declares a local `auto result = 1;`.
  Verified to fail without the guard (`let indexed = if true` in its place) and pass with it.

### 8. `LSPHandle.cpp` cannot see `LanguageServer.h` — a bare quoted include and no compile database

* **Reported from use** (picture 1): `_server->GetService<ConfigService>()` is underlined, and the hover says
  "`LanguageServer*` has no member `GetService`".
* **Measured**: the class is fine and its facts are fine (`facts --file CodeFormatServer/src/LanguageServer.h` lists all
  30, including `44:14 GetService function in LanguageServer returns Service *`), but
  `members --class LanguageServer --file <LSPHandle.cpp> --index` answers **0 members** while asking from the header
  itself answers **21**. The cause is one line of the summary:

  ```text
  include "LanguageServer.h" -> unresolved        ← the other 16 resolve
  ```

  The project has **no `compile_commands.json` and no `.cppls.toml`**, so the search paths are the toolchain's only,
  and a **quoted include with no directory** is exactly the case the fuzzy fallback refuses by contract
  (`include::fuzzy::tests::a_bare_file_name_is_never_matched`). With the include directories stated,
  `include "LanguageServer.h" -> .../CodeFormatServer/src/languageserver.h` and `members` answers **21**, including
  `GetService`. A `.cppls.toml` naming the project's own `-I` directories is in that project now.
* **Fixed in the resolver, not in the project.** `IncludeResolver::resolve`'s fuzzy fallback now answers a **quoted**
  include with no directory when exactly one file under the root has that name
  (`FuzzyIndex::the_only_file_named`) — one file is that file, two are an ambiguity and answer nothing, which is the
  same rule the tail matcher applies. An **angled** include keeps the old refusal: `<vector>` is spelled without a
  directory because the search path is meant to find it, and a project file named `vector` is not the standard
  library.
* **Measured after, with the project's `.cppls.toml` removed and its cache deleted** — i.e. the state the report came
  from: `17 include(s) (17 resolved)`, `include "LanguageServer.h" -> .../CodeFormatServer/src/LanguageServer.h`,
  `members_of(LanguageServer)` = **21**, and the hover on `_server->GetService<ConfigService>()` answers
  `` `LanguageServer::GetService` — 2 declarations `` instead of "`LanguageServer*` has no member `GetService`".
* **Pinned by** `include::tests::fuzzy_fallback::a_bare_quoted_include_finds_the_only_file_of_that_name` and
  `…_with_two_files_of_that_name_is_unresolved`, and the old bare-name contract became
  `a_bare_angled_name_is_left_to_the_search_path` (its `c/stdio.h` is unique under that root, and the angled spelling
  still does not take it).


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
