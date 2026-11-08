# The bug list

Two kinds of entry: **a wrong answer**, which is a fact about the analysis, and **a measured cost**, which is a fact about
the machine. Each entry says how it was measured, so the next reader can reproduce it rather than trust it.

## Open

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

### 2. Editing a file re-parses and re-indexes the whole expansion

* **Reported from use.** "只要我编辑代码，当前文件就会有较长时间的卡死".
* **Cause.** A file's reading is the whole **stitched** stream — its own text plus every `#include`'s text, macro-expanded
  — and an edit reparses and reindexes that entire stream even though the include region has not changed.
* **The shape of the fix** is the reader's own proposal and it is right: one file is three artifacts, not one —
  the **include region's tree** (rarely changes), the **file's own region's tree** (changes on every edit), and the
  file's **lex stream** (already exists, and is what a keystroke's queries mostly need). See
  [`parse-architecture.md`](parse-architecture.md).
* **Progress.** The parser now parses a **token range**, not only a whole text
  (`CppParser::parse_tokens`, step 1 of that document, four properties pinned in `tests/token_range.rs`). What is left is
  the reader's side: the stitcher hands over each region's token range, and the queries are rebuilt on the own region.

### 3. Opening a file costs the whole closure before any query answers

* **Reported from use.** "我新打开的文件会导致CPU轰鸣，当前页面需要长时间的加载，导致其余LSP功能几乎无法响应".
* Same cause as #2 from the other end: the first read builds the whole stitched stream and indexes it before the
  file's own answers exist. The fix is the same split, plus answering the file's own region from its lex stream first.

## Closed

### An object's own parameter list was read as a function's

* `LuaParser p(file, std::move(luaLexer.GetTokens()));` — `p.HasError()` answered "`p` is a `LuaParser(file, std::move)`,
  the class is out of the index". The type reader wrapped the object's constructor call into `Type::Function`; the fact
  builder stripped it afterwards and the expression engine did not.
* Fixed in `7a1993a`: the reader is told whether the declaration declares an object, and an object's own list is its
  constructor call. Measured after: `type_at(p) = "LuaParser"`, `members_of(LuaParser)` = 71 members including
  `HasError`.
