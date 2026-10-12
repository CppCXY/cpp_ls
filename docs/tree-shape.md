Two tree-shape defects, their exact causes, and why the fix is not a small one

Reported by reading the tree for `struct String;` and `struct ContainerProxy { const String* container; };`:

  a forward declaration's name is a `TemplateType`
  `DeclSpecifierSeq` covers far more than a decl-specifier-seq
  every node absorbs its trailing trivia

All three are real and all three are measured below. The first is a **symptom**, the second is the
**cause**, and the third has a **documented reason** that the fix has to respect.

================================================================================
1. THE CAUSE: a class definition is parsed INSIDE the decl-specifier-seq
================================================================================

  struct S { int n; };
    Declaration
      DeclSpecifierSeq "struct S { int n; }"     <- covers the whole definition
        StructDef
          Token(StructKeyword) "struct"
          NameExpr "S"
          ClassBody "{ int n; }"

`DeclSpecifierSeq` is documented as *"the `decl-specifier-seq`: type specifiers, cv-qualifiers,
storage class and function specifiers, in any order"* — and the standard's grammar says a
`decl-specifier-seq`'s type-specifier may be a class-specifier, so containing the definition is
defensible. What is not defensible is the **span**: it says the declaration's specifiers run to the
end of the class body, so a reader taking "the text of the specifiers" gets the whole definition.

The mechanism is one line in `types.rs`: `parse_class_like_head` is called from inside
`parse_one_decl_specifier_inner`, which runs **while the `DeclSpecifierSeq` mark is open** — so the
class node is created as its child rather than beside it.

Where it hurts, measured through the fact builder:

    struct String;                Type "String" range 0..15 = "struct String;\n"
    struct S { int n; };          Type "S"      range 0..19 = "struct S { int n; }"
    struct B : A { int n; };      Type "B"      range 13..36 = "struct B : A { int n; }"
    template <class T> struct V;  Type "T"      range 10..17 = "class T"

So a class's own fact covers its whole definition — including the member declarations — and a
template *parameter* is filed as a `Type` whose range is `class T`. The first is why a comment above
a member cannot be attributed, and the second is spurious on its face.

================================================================================
2. THE SYMPTOM: `TemplateType` for a name that has no template arguments — **已修**
================================================================================

  struct String;
    DeclSpecifierSeq
      BuiltinType        "struct "
      TemplateType       "String"        <- no `<` anywhere
        NameExpr "String"

`TemplateType` is what the parser produced for **every** named type in a specifier sequence, so a bare
`struct String` read as a template instantiation. The parser's own note on
`CppDeclSpecifierSeq::get_type_name` described the right shape — *"a plain name is a `NameExpr` child, while a
template-id (`std::vector<int>`) is wrapped in a `TemplateType`"* — and the implementation wrapped both.

### 修法与它的两次失败

**第一次尝试(错):** 去掉包装,让普通名做裸 `NameExpr`。四个测试当场否掉:

```text
the_last_name_in_a_specifier_sequence_is_the_type
a_forward_declaration_declares_the_name
a_member_whose_type_is_preceded_by_macros_is_named_by_its_declarator
a_word_in_a_body_that_expands_to_nothing_does_not_stop_the_reading
```

**原因是量出来的,不是猜的:`TemplateType` 在解析器内部是承重的。**
`parse_decl_specifier_seq_with` 用 `events_contain_any(specifiers_from, &[TemplateType])` 回答
**"类型名写了没有"** —— 这个答案决定 MSVC 那行

```cpp
extern "C++" __PURE_APPDOMAIN_GLOBAL _CRTDATA2_IMPORT istream cin;
```

里第二个名字是并进类型还是声明 `cin`。拆掉节点 = 拆掉这个判据。

**第二次(对):** 给"具名但非模板"自己的 kind —— `CppSyntaxKind::NamedType`,两个问题就分开了:

```text
"这里命名了一个类型吗"   → TemplateType | NamedType
"它是模板 id 吗"         → TemplateType
```

七处读者随之接受两种 kind:`a_further_name_may_join`、`get_type_name`、`scopes.rs` 两处、
`sema/types.rs` 两处、`sema/scopes.rs` 的 wrapper 匹配。修完:

```text
struct String;                DeclSpecifierSeq > BuiltinType "struct " > NamedType "String"
std::vector<int> v;           DeclSpecifierSeq > TemplateType "std::vector<int>"
Widget w;                     DeclSpecifierSeq > NamedType "Widget "
```

595 lib + 全部 24 个集成套件 + `cpp_parser` 89 全绿,四个曾被否掉的契约全部恢复。


================================================================================
3. TRAILING TRIVIA, and why the fix is not "stop absorbing it"
================================================================================

Every node's span ends after the whitespace that follows its last token:

    BuiltinType  "struct "          the space is inside
    NameExpr     "container"         ... but in ClassBody the name took its trailing space too
    DeclSpecifierSeq "struct S { int n; }"      no trailing newline here, because the `;` follows

**This is a deliberate choice, documented in the grammar**, and its counterexample is instructive —
`parse_access_specifier`, `decls.rs:4455-4467`:

  "`bump` attaches the trivia *after* a token to whichever node is open, which is usually what you
   want: `int  x` keeps its spaces together. For an access specifier it is not. Everything after
   `public:` — the newline, an indented `/// doc`, the member below — would then be a child of the
   access specifier rather than of the class body, and a consumer asking 'which comment documents
   this member?' would find nothing, because the only comment in the class body is the one the
   access specifier swallowed."

So the grammar already knows the failure mode and fixes it **per node, by closing the node before
the trivia is emitted**. A blanket change to stop absorbing trailing trivia would break the reason
that fix exists — `int  x` would lose the association between a token and the space that documents
it — and it would touch every node in a tree that is deliberately lossless.

================================================================================
4. WHAT THE FIX IS, and why it is not one line
================================================================================

For (2)/(1), the shape that matches the documentation: the class-key is **one specifier** of the
sequence, and the definition is read where a declaration's declarator goes, so

    Declaration
      DeclSpecifierSeq "struct "
      StructDef        "S { int n; }"

The obstacle is measured, not assumed: `DeclSpecifierSeq` has **~60 readers**, and eleven of them are
in `sema/scopes.rs` — the code that decides which declarations are filed where. `class E { … }`
must still declare `E` as a type, and `enum class E : unsigned char` writes its underlying type
inside the same specifier, so the head's own parsing is load-bearing for name lookup. Moving the
definition out of its parent is therefore a change to how the scope builder sees every class in the
project, and the suite that would catch a mistake is the corpus that must stay at zero false
positives.

For (3), the fix is the access-specifier pattern applied where it matters: close the node before its
trailing trivia, for the nodes whose span is read as *text* — the specifier sequence and the type
nodes — rather than everywhere.

================================================================================
5. WHAT TO MEASURE FIRST
================================================================================

Before changing either, the question that decides the order: **which of the two actually misleads a
reader today?** (1) is visible in the fact builder's ranges above; (3) is visible wherever a node's
text is compared, and `status.md` §5's four recorded defects were all of that kind. A census of
"which readers take a node's text rather than walking it" is the measurement that says whether (3)
is worth the risk — and `grep` for `.text()` over the two crates is where it starts.
