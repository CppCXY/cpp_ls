# The inference defects behind the LSPHandle.cpp reports

A reader hovering `E:\EmmyLuaCodeStyle\CodeFormatServer\src\LSP\LSPHandle.cpp` reported six things at once: hover feels
very slow; a constructor shows its declaration **and** its definition; `_server->GetService<CommandService>()` types
wrongly and nothing after it resolves; `std::filesystem::exists` has no hover; `auto opSyntaxTree = vFile.GetSyntaxTree
(vfs);` types as something else; and `auto totalLine = lineIndex->GetTotalLine();` "could not be typed".

They are **three defects plus one measurement**, and the measurement is the reason they looked unrelated: one wrong
type propagates to every variable fed by it, so four hover cards fail from one root. The instrument is
`tests/one_file_probe.rs` — one file, opened the way the server opens it, with `type_at` / `definitions` / members
printed at named places, and every phase timed.

```powershell
$env:CPPLS_PROJECT_ROOT = "E:\EmmyLuaCodeStyle"
$env:CPPLS_FILE = "E:\EmmyLuaCodeStyle\CodeFormatServer\src\LSP\LSPHandle.cpp"
$env:CPPLS_NEEDLES = "totalLine|opSyntaxTree|GetService"
$env:CPPLS_INDEX_THE_PROJECT = "1"
cargo test --release -p cpp_code_analysis --test one_file_probe -- --ignored --nocapture
```

## 1. A reference in the initializer survived into `auto`

```text
auto &vfs = _server->GetVFS();      // GetVFS() returns VirtualFileSystem&

before   vfs            VirtualFileSystem&&
after    vfs            VirtualFileSystem&
```

`auto_substituted` appended the declarator's `&` to the initializer's type *as written*, reference and all. C++ says a
reference never survives into `auto`, so `auto&` on a `T&` initializer is `T&`. The `&&` answer names **no class**, so
`vfs.GetVirtualFile(...)` answered `Unknown(NotDeclaredHere("VirtualFileSystem::GetVirtualFile"))`, and `vFile`,
`opSyntaxTree`, `lineIndex` and `totalLine` each inherited that refusal. Four hover cards, one line.

`auto&&` now collapses instead of being refused: `T&` stays `&`, `T`/`T&&` becomes `&&`, and where the initializer's
*shape* decides its value category (a literal, a call) that is used. A plain name still answers `&&` — recorded as a
gap in `a_forwarding_reference_collapses`, not as the language's rule.

## 2. A call of a member function was read as a call of an object

```text
before   opSyntaxTree    value_type
after    opSyntaxTree    std::optional<LuaSyntaxTree>
```

This model has no function type: the type of a **member function's name** is what the member *returns*. So the "does
this callee declare an `operator()`?" arm of `type_of_a_call` fired on every member call returning a class, and the
answer was `std::optional`'s `operator()` return — `value_type`. The arm now runs after the callee's declaration is
resolved, and only when that declaration is not a function.

## 3. `ptr->Member<T>()` read the template argument as the member name

```text
before   _server->GetService<CommandService>()    Unknown(NotDeclaredHere("LanguageServer::CommandService"))
after                                             Service*          ← the member is found, its argument is not substituted
```

`member_access_of` took the **last** identifier in text order after the operator. That is right for `a.b.c` (whose
object's identifiers are inside a node) and wrong for the member's own template arguments, which follow the name in the
same node. It is now the **first** identifier after the operator.

The arrow itself also resolves through `completion::the_class_after_the_operator` in `declaration_of_a_callee` now, so
`shared_ptr<LineIndex>->GetTotalLine()` asks `LineIndex` rather than the smart pointer.

## What is still wrong, with the measurement that says where

**`lineIndex->GetTotalLine()`** — `totalLine` is still `Unknown(NotDeclaredHere("std::shared_ptr::GetTotalLine"))`, and
the trace names the exact step:

```text
element_type: std::shared_ptr is not pointer-like (15 member(s), operators ["operator=", "operator=", …])
members of std::__shared_ptr: 0
```

`shared_ptr` inherits `operator->` from `__shared_ptr`, and **the raw reading of `bits/shared_ptr_base.h` gives that
class zero members**. So the pointer protocol cannot be recognised, the argument fallback is never reached, and the
arrow stops at the smart pointer. The fix is not another heuristic here: it is that a *closure header's* reading must
be complete — the same "raw readings of standard headers are incomplete" problem that cooking fixes per file, and
`shared_ptr` is never the open file.

**`GetService<CommandService>()`** answers `Service*`: the fact is `returns "Service *"`, `parameters ["Service"]`, and
the substitution in `what_a_call_has` is guarded on `written_arguments`, so `written_template_arguments(callee)` is
returning nothing for a **member** call. That is where the next hour goes.

**Hover feels slow** is not the question path: measured on this file, `Session::open` 40–150 ms, `catch_up` 70–660 ms,
`cook` 7 ms, `view` 2 ms, and each hover question **0–3 ms**. What is slow is the pump beside it, and one number says
how slow it can get: `index_everything` — which cooks **every** file's closure — takes **9.7 GB and more than twenty
minutes** on this project and never finishes. A server does not call it, but anything that does (a probe, a batch tool,
a test) meets the same wall, and it is the shape of "I do not know what it is waiting for".

**The duplicate hover card** (`LSPHandle::LSPHandle` shown twice, once from the header) is the card listing every
declaration it found: a constructor's declaration and its definition are two facts, and `Ambiguous` is answered by
`session.definitions`, which returns both. It is a presentation rule, not an inference one — and it is the last of the
six reports still untouched.
