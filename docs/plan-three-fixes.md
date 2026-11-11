# Three fixes, specified before they are written

The rule for all three: **the tree decides every question about this file, and the index decides the cross-file one it
was built for.** A text peek is a workaround; so is "the class the cursor happens to be in" when the declaration says
which class it is. Each item is a fixture first, then the change, then the workspace suite and clippy, one commit.

## 2. A member is identified by its declaration, not by where the cursor is

**Today**: `name_level_offsets` narrows with `the_class_the_cursor_is_in` — the innermost class scope around the
*cursor*. The class a *use* is written in is not the class it refers to: `diagnosticBuilder.SpellCheck(...)` inside
`LuaCodeFormat::NameStyleCheck` is written in `LuaCodeFormat` and names a member of `DiagnosticBuilder`, so the filter
lets it through and Find References mixes two classes' members.

**The rule**: a member use is attributed by the **type of its object**; a class-body use by the **enclosing class**; any
other use by its qualified name. That is C++'s lookup, and each of the three cases is a node:

| occurrence | node | the answer comes from |
|---|---|---|
| `obj.member`, `p->member` | `CppMemberExpr` (object + name) | the object's type — `a_member_of_an_instance` / `member_across_files` |
| `member` inside a class body or a member function | the scope chain | that class's members |
| anything else | `CppNameExpr` | qualified-name lookup, as today |

**Work to do**

1. `cpp_code_analysis`: one public query — `member_references(index, files, class_qualified, member_name, budget)` —
   whose occurrence filter is the table above, reusing the index's member lookup rather than re-deriving types here.
   The target's class comes from the target fact's `qualified_name()`, never from the cursor.
2. `cpp_ls`: `name_level_offsets` asks for the symbol by **fact** (as `symbol_references_of` already does) and hands the
   query the fact's qualified name; `the_class_the_cursor_is_in` stops being an input to attribution.
3. Fixture: two classes with a same-named member, a call on each, plus a call on an instance of the *other* class from
   inside the first — Find References, rename and the CodeLens count all answer 1 use, not 3.

**Why not keep the cursor's class**: it is a question about the use site, and the answer is a property of the
declaration. It is also why the CodeLens and Find References disagreed: the lens had the fact and the list did not.

## 1. A name in a member position is a field, and its binding is the AST's name

**Today**: `: _supportNonStandardSymbol(false)` draws that name as a function. Two separate questions are being answered by
one rule: *is this a call?* (the initialiser list looks like one) and *what is this name?* (a member of the class being
constructed).

**The rule**: the declarator's `name` is the last segment of the AST's name node — `qualified_name()` already splits it —
never a `rsplit("::")` on the declarator's text, and a name in a mem-initialiser is bound as a **field of the enclosing
class**, because that is what [class.base.init] says it names.

**Work to do**

1. `sema/scopes.rs`: `is_a_constructor_declarator` and the constructor/field binding name both read `qualified_name()`
   segments from the tree instead of `declarator.text()`. (The `X::X` rule then also handles `X< T >::X` and a newline
   before `::` for free.)
2. Bind each mem-initialiser's name in the class scope as a `Field`/`Variable` binding when the class declares one, so
   the classifier's step 1 finds a name in that position at all.
3. Fixture: the constructor with `: _support(false)` — the qualifier is `Type`, the initialiser's name is a
   `Property`/member, and an ordinary call in the same list is still a `Function`.

## 5. A class template's member types are substituted, not guessed

**Today**: `auto &result = results.emplace_back();` leaves `result` unknown. `emplace_back`'s return type `reference` is a
member alias of `std::vector<T>` — a **dependent** type, and this layer substitutes function-template parameters only.

**The rule**: an alias declared in a class template is substituted with the template's arguments when the class
instantiation is known: `vector<LuaDiagnosticInfo>::reference` → `value_type&` → `LuaDiagnosticInfo&`.

**Work to do**

1. `index/project.rs`: when a call's callee is a member of an instantiated class (`results` has type
   `std::vector<LuaDiagnosticInfo>`), look the member's declared return type up **through the class's own aliases**
   (`using reference = value_type&;`) with the class arguments substituted; one function, `member_return_type`, that
   takes the instantiated type rather than a spelling.
2. The substitution itself reuses `substitute` (the function-template path), because the operation is the same one: a
   declared text with parameters, and an environment that maps them.
3. Fixture: `std::vector<int> v; auto &r = v.emplace_back(); r = 3;` — `r` is `int&`; and the reported shape
   `results.emplace_back().Start` resolves `Start` through `LuaDiagnosticInfo`.

**What it is not**: a table of member names per container. The alias chain is in the class's own facts; a project class
with `using reference = T&;` must answer the same way, and a table would only answer for the library.
