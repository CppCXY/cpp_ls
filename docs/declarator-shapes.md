# The declarator's shape, measured — and fixed

Written after the round that fixed the parenthesised declarator's parameter list (`LUA_API void (lua_setfield) (…)`)
and asked whether the tree that needed that fix is the tree it should be. It was **not**, and this is the measurement
that said so plus the change it led to.

The instrument is `crates/cpp_parser/tests/declarator_shapes.rs` — `cargo test --release -p cpp_parser --test
declarator_shapes -- --nocapture` prints the table from the grammar itself, and its tests pin the invariant.

## What the grammar says

```text
declarator        ptr-declarator
ptr-declarator    noptr-declarator | ptr-operator ptr-declarator
noptr-declarator  declarator-id
                | ( ptr-declarator )
                | noptr-declarator ( parameter-declaration-clause )
                | noptr-declarator [ constant-expression ]
```

Two consequences matter here, and they are the whole of this note:

1. A suffix — the parameter list, the array bound — is a child of the **`noptr-declarator` it follows**, so it is a
   child of *some* declarator in the chain, and the one it belongs to is the one whose other child is the name (or the
   parenthesised group that holds the name).
2. `( declarator )` is a `noptr-declarator` like any other, so the suffixes that follow it belong to it, not to the
   declaration around it.

## What the tree was, and what it is

`shape` is the chain of the `InitDeclarator`'s children; `get_param_list()` is the declaration-level answer.

| declaration | before | after |
| --- | --- | --- |
| `void simple(int a);` | `Declarator[simple(int a)]` ✓ | unchanged |
| `int *ret(int);` | `Declarator[*ret(int)]` ✓ | unchanged |
| `void (named) (int a, char *b);` | `Declarator[(named)] ParameterList[…]` | `Declarator[(named) (int a, char *b)]` |
| `int (*fp)(int);` | `Declarator[(*fp)] ParameterList` | `Declarator[(*fp)(int)]` |
| `int (**ppf)(int);` | sibling list | `Declarator[(**ppf)(int)]` |
| `int (*maker(int a))(int b);` | sibling list | `Declarator[(*maker(int a))(int b)]` |
| `int (*table[3])(int);` | sibling list | `Declarator[(*table[3])(int)]` |
| `void (Widget::*pm)(int);` | sibling list | `Declarator[(Widget::*pm)(int)]` |
| `void (*signal(int, void (*)(int)))(int);` | sibling list | `Declarator[(*signal(int, void (*)(int)))(int)]` |
| `void takes(int (&arr)[3]);` | ✓ | unchanged |
| `typedef int (*Fn)(int);` | ✓ | unchanged |
| `auto trailing(int a) -> int;` | ✓ | unchanged |

## The two defects the old shape caused

**1. A suffix after `( ptr-declarator )` was a sibling of the declarator.** It is why `CppDeclarator::get_param_list`
had grown a rule that looked at the parent — right for one shape (`(named)(int)`, where the list really is the name's)
and wrong for the rest, where the list belongs to a *type*.

**2. A function returning a function pointer was recorded as a variable.**

```text
before   int (*maker(int a))(int b);   `maker  variable type int(int)*`
after    int (*maker(int a))(int b);   `maker  function parameters (int a)`

before   void (*signal(int, void (*)(int)))(int);   variable
after    void (*signal(int, void (*)(int)))(int);   function
```

and the leaked parameter:

```text
before   `b` (from the returned pointer's `(int b)`)   a file-scope variable
after    not declared — it is the type's, not the function's
```

## The change

1. **The grammar attaches the suffixes to the declarator that opens at `(`** — in `parse_init_declarator`'s
   parenthesised branch, `finish_declarator_suffixes` runs before `paren.complete(p)`; and
   `parse_parenthesised_declarator` reads its own suffixes for the other entry points. The suffix loop's
   "is this a parameter list or an initialiser" decision needs `last_declarator_is_function`, so that flag is set
   *before* the suffixes are read.
2. **`CppDeclarator::get_param_list` is a direct child again** — the sibling rule is gone.
3. **`CppDeclaration::get_param_list` walks for the declarator that owns both a name and a list** — the grammar's
   `noptr-declarator: declarator-id ( params )` — and falls back to the outermost list when no declarator has a name
   beside it (`(named)(int)`).
4. **`sema::scopes`'s function-or-variable question is structural**: rise from the declarator that *names* the binding,
   list first and **stopping at a pointer/reference** (a list beyond one is a type's), and the same walk collects the
   function scope's parameter lists — which is what stopped `b` being filed at file scope. The reading it replaced
   looked at the **text**: a declarator starting with `(*` or `(&` was a pointer to a function.

## What was re-checked

```text
sema::declarations::declared_parameters_with   ✓ the facts' parameter_list
sema::scopes::parameter_lists / the kind       ✓ the function scope's parameters, function-vs-variable
sema::scopes::promises_no_return / get_body    ✓ unchanged (a body is a declaration child)
call_syntax::{parameter_list_of, parameter_list_text}   ✓ the name-anchored rise they already had
index::project::type_of / calls                ✓ (the type spellings improved: `int (*)(int)` where it was `int(int)*`)
```

The guards:

```text
cargo test -p cpp_parser -p cpp_toolchain -p cpp_code_analysis   all green
cargo test --release -p cpp_ls --tests                          all green
crates/cpp_code_analysis/tests/c_declarators.rs                 the facts: kind, parameters, the two chains that type
crates/cpp_parser/tests/declarator_shapes.rs                    the shape invariant, per declarator
```

And on Lua 5.5.0 — the project the reports came from: `lua_setfield`'s fact keeps its parameters, the hover card reads
the real signature, `lauxlib.c` places its 301 parameter hints.

**The type half, fixed in the same round:** the *return* type of a function returning a function pointer now reads the
shape rather than the specifiers — `a_parenthesised_return_type` in `sema::declarations` takes the operators written
between the declarator's `(` and the name, and everything after the name's own parameter list:

```text
before   int (*maker(int a))(int b);            `returns int`
after    int (*maker(int a))(int b);            `returns int (*)(int b)`

before   void (*signal(int, void (*)(int)))(int);   `returns void`
after    void (*signal(int, void (*)(int)))(int);   `returns void (*)(int)`
```

`const char *(luaL_tolstring)(int)` and `Widget *make()` keep their spellings (`const char *`, `Widget *`), which is
what the ordinary assembly answers — the parenthesised reading is only for the shape it names.

## What not to do

- **Do not search descendants for a `ParameterList`.** `int (*f(int))(int)` has two, and the first found is the wrong
  one half the time. The rule is positional: the list that is a *sibling of the name* under the same declarator.
- **Do not decide function-or-variable from the declaration's text.** The prefix `(*` is the same for a pointer
  variable and for a function returning a pointer, and one of the two is a function.
