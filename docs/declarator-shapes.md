# The declarator's shape, measured

Written after the round that fixed the parenthesised declarator's parameter list (`LUA_API void (lua_setfield) (…)`)
and asked whether the tree that needed that fix is the tree it should be. The answer is **no**, and this is the
measurement.

The instrument is `crates/cpp_parser/tests/declarator_shapes.rs` — `cargo test --release -p cpp_parser --test
declarator_shapes -- --nocapture` prints the table below from the grammar itself, and its tests pin which half of the
split each shape is on.

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

## What our tree does

`get_param_list()` is the declaration-level answer, and `shape` is the chain of the `InitDeclarator`'s children.

| declaration | shape | `get_param_list()` |
| --- | --- | --- |
| `void simple(int a);` | `Declarator[simple(int a)]` | `(int a)` — a child ✓ |
| `int *ret(int);` | `Declarator[*ret(int)]` | `(int)` — a child ✓ |
| `void (named) (int a, char *b);` | `Declarator[(named)] ParameterList[…]` | the list is a **sibling** |
| `int (*fp)(int);` | `Declarator[(*fp)] ParameterList` | **None** (correct here — a pointer is a variable, and the list is the type's) |
| `int (**ppf)(int);` | `Declarator[(**ppf)] ParameterList` | **None** (correct, same reason) |
| `int (*maker(int a))(int b);` | `Declarator[(*maker(int a))] ParameterList` | **None** — *wrong*: `maker` is a function with `(int a)` |
| `int (*table[3])(int);` | `Declarator[(*table[3])] ParameterList` | **None** (correct: a variable, array of pointers) |
| `void (Widget::*pm)(int);` | `Declarator[(Widget::*pm)] ParameterList` | **None** (correct: a member-pointer variable) |
| `void (*signal(int, void (*)(int)))(int);` | `Declarator[(*signal(…))] ParameterList` | **None** — *wrong*, same as `maker` |
| `void takes(int (&arr)[3]);` | `Declarator[takes(int (&arr)[3])]` | the list — a child ✓ |
| `void takes2(int (*)(int));` | one declarator | ✓ |
| `typedef int (*Fn)(int);` | `DeclSpecifierSeq[int] Declarator[(*Fn)(int)]` | the list is inside ✓ |
| `using Fn2 = int (*)(int);` | `NameExpr[Fn2] TypeId[…]` | n/a |
| `auto trailing(int a) -> int;` | one declarator | ✓ |
| `struct S { int (*m)(int); };` | the class body | n/a |

## The two defects

**1. A suffix after `( ptr-declarator )` is left as a sibling of the declarator.** Six of the shapes above do it. It is
why `CppDeclarator::get_param_list` had to grow a rule that looks at the parent — and that rule is right for exactly
one of the six (`(named)(int)`, where the list really is the name's) and wrong for the other five, where the list
belongs to a *type* (a pointer's pointee, an array's element, a member pointer's class). Today it is saved only because
it refuses when the declarator holds a `*`/`&`.

**2. The consequence is not only a missing list: a function returning a function pointer is recorded as a variable.**

```text
int (*maker(int a))(int b);   fact: `maker variable type int(int)*`
int (*fp)(int);               fact: `fp    variable type int(int)*`
```

`maker` **is** a function (`(int) -> int(*)(int)`), and the fact says it is an object of function-pointer type, so its
calls, its signature help and its hover are all answering about an object. `void (*signal(int, void (*)(int)))(int);`
— the declaration every C textbook uses to explain declarators — is the same.

## The change, and why it is a change rather than a workaround

The grammar produces the tree; the accessors then guess. Guessing cannot fix (2), because the *fact builder* decides
"function or variable" from the binding's kind, which comes from the declarator chain — and the chain has no
`ParameterList` where the name's own function type is.

So the change is the grammar's own rule, in two steps:

1. **Attach the suffixes to the declarator that opens at `(`** — i.e. `parse_parenthesised_declarator` reads the
   suffixes before closing its own node, which is `noptr-declarator: ( ptr-declarator ) suffix*`. Every shape in the
   sibling column then has the list *inside* the declarator, beside the parenthesised group.
2. **Ask the parameters of the declarator that holds the name**, not of the outermost one. With the fixed tree,
   `maker`'s `(int a)` is a direct child of the declarator whose other children are `*` and `maker`, and the pointer's
   `(int b)` is a child of the declarator one level out. That distinction is what `get_param_list` must make, and it is
   the rule the grammar states: `noptr-declarator: declarator-id ( params )`.

The sibling rule in `CppDeclarator::get_param_list` then goes away, and `CppDeclaration::get_param_list` becomes "the
declarator that names the declaration, its own list".

**What has to be re-checked when it lands**, because they read the same shapes:

```text
sema::declarations::declared_parameters_with   the fact's parameter_list
sema::scopes::the_parameters_of                a function scope's parameters (its bindings)
sema::scopes::promises_no_return / get_body    a `{` after the declarator is a body
sema::types::declarator_declaring               which declarator declares a binding
call_syntax::{parameter_list_of, parameter_list_text}
index::project::type_of / calls                `is_a_pointer`, `get_pointer_types`, the member questions
```

The guards are this survey (a table a person reads), `crates/cpp_code_analysis/tests/c_declarators.rs` (the facts:
kind, parameters, the two chains that now type), and the whole suite — 190 test binaries, 41 server binaries.

## What not to do

- **Do not search descendants for a `ParameterList`.** `int (*f(int))(int)` has two, and the first found is the wrong
  one half the time. The rule has to be positional: the list that is a *sibling of the name* under the same
  declarator.
- **Do not keep the sibling rule "for compatibility".** It answers a type's list as if it were the name's; the only
  reason it looks right today is the guard that refuses pointer declarators, and that guard is what makes `maker` a
  variable.
