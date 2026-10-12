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

### 试过并**回退**的两次,以及它们量出来的东西

**第一次(边界外移,计数器没跟着走)。** 在类体之前关闭 `DeclSpecifierSeq`、之后重开。
实测:

```text
LOOP enter call=0x…f08 specifiers=0 token=StructKeyword text="struct"
LOOP enter call=0x…9a8 specifiers=0 token=IntKeyword    text="int"      <- 类体内部
LOOP enter call=0x…9a8 specifiers=1 token=Identifier    text="n"
LOOP err   specifiers=1 -> expected a declarator name
LOOP enter call=0x…9a8 specifiers=0 token=Identifier    text="n"
LOOP enter call=0x…9a8 specifiers=1 token=Semicolon     text=";"        <- 内层的 `;`
LOOP err   specifiers=1 -> expected a type specifier
LOOP reopened at specifiers=0
LOOP enter call=0x…f08 specifiers=1 token=Semicolon     text=";"        <- 外层 +1 了
LOOP err   specifiers=1 -> expected a type specifier
TOTAL errors: 1
```

根因**不是**重入:每次调用有自己的 `specifiers`。是**外层循环在重开之后仍然执行了
`specifiers += 1`**,于是外层的 `;` 看到 1,`if specifiers == 0 { break }` 不成立,
错误被返回而不是结束序列。改成重开后 `continue`(那个 specifier 已经在刚关闭的序列里记过了),
外层就正确地在 `;` 处 break。

**第二次(计数器修对之后,树更糟)。** 同一个探针:

```text
struct S { int n; };       ->  expected a type specifier at ";"   TranslationUnit > ErrorNode "}" / EmptyStat ";"
struct S { int n; } s;     ->  expected a declarator name at "s"  ErrorNode "} " / ExpressionStat "s;"
struct { int a; } x;       ->  expected a declarator name at "x"  同上
template <class T> struct V { T x; };  -> ErrorNode "}" / EmptyStat ";"
struct String;             ->  expected a type specifier at ";"
```

**整个声明塌成了 `ErrorNode` + `EmptyStat`** —— 类定义没有被认成声明的一部分。
也就是说:**声明解析器依赖那个 `StructDef` 在 `DeclSpecifierSeq` 里面。** 只知道"定义应该
是兄弟"是不够的:声明那一侧必须同时学会在 specifier 序列**之外**接住一个类定义
(`struct S { … } s;` 的 `s` 就是第一个证据 —— 它现在被读成了表达式语句 `s;`)。

结论:**这不是"把节点挪出去"的问题,是声明解析器与 specifier 序列之间的分工要一起改。**

================================================================================
4b. 入口已经找到:`parse_declaration_here` 与那个类定义的三处接缝
================================================================================

`crates/cpp_parser/src/grammar/cpp/decls.rs`,函数 `parse_declaration_here`(645),类定义那一段:

```text
1027   let specifiers_from = p.current_event_count();
1029   parse_decl_specifier_seq(p)                       <- 这一个调用吃掉了关键字**和整个类体**
…
1073   if p.current_token() == LeftBrace
1074       && (declaration_opens_a_body(p)
1074           || declaration_wrote_a_class_head(p, specifiers_from))
1076       parse_class_body(p)                            <- body 规则
1101       if Semicolon { p.bump() }                      <- 类定义的那个 `;`
1103       else if a_declaration_cannot_follow(p) { … "expected `;` after a class definition" }
1110       return Ok(m.complete(p));
```

**`parse_decl_specifier_seq` 返回时,光标已经在整个 `struct S { … }` 之后了。**
类定义是那一次调用里的一个 specifier(`parse_one_decl_specifier` 的
`is_class_like_keyword(kind) && a_body_follows_the_class_head(p) => parse_class_like_head(p)`),
所以 `DeclSpecifierSeq` 的 span 必然覆盖它 —— **不是节点放错了位置,是它由一次调用产生**。

三处接缝,每一处都是"这个节点必须在那里面"的理由:

```text
① declaration_defined_a_class(p, specifiers_from)      decls.rs:1199
     events_contain_any(specifiers_from, &[ClassBody, CompoundStat])
     —— 用来判断"`;` 属于这个声明(`class Foo {};`)还是跟在后面的空语句"。
     判据是**事件**,不是节点归属,所以定义挪出去这个判据仍然成立。

② declaration_wrote_a_class_head(p, specifiers_from)   decls.rs:1212
     events_contain_any(specifiers_from, &[ClassDef, StructDef, UnionDef, EnumDef, EnumClassDef])
     —— 用在 1073:`{` 是否是这个声明里那个头的 body(为 `#if` 把头和 body 分到两边而写)。
     同上,也是事件判据。

③ 因此真正的冲突只有一处:
     `parse_one_decl_specifier` 的**成功**表示"一个 specifier 读完了",而类定义那一次
     既读完了 specifier(struct),又把 body 一起读掉了。外层循环的 `specifiers` 计数器
     和 `DeclSpecifierSeq` 的 mark 都假设"一次调用 = 一个 specifier 节点"。
```

**所以"边界外移"要成功,必须让那个类定义不再是一次 specifier 调用的产物的同时,
仍然让 ①② 看得见它** —— 它们是事件判据,这一点是有利的;要改的是
`parse_class_like_head` 的调用点:类定义应当由 `parse_declaration_here` 在
`parse_decl_specifier_seq` **之外**读,而不是由 specifier 循环读。

**下一步的具体形状**(还没做):

```text
1. `parse_one_decl_specifier` 遇到 `is_class_like_keyword && a_body_follows_the_class_head` 时
   **不再**调用 `parse_class_like_head`,只消费关键字并返回一个"轮到这个声明去读类定义"的信号。
2. `parse_decl_specifier_seq` 收到信号后立即结束(它已经是一个完整的 decl-specifier-seq:
   `struct`),把信号交给 `parse_declaration_here`。
3. `parse_declaration_here` 在 1073 那一段之前先读类定义(它已经会读 body —— 1076),
   再走既有的 `;` / declarator 分支。
4. `struct S { … } s;` 的 `s` 走 1116 的 init-declarator 循环,这也是它现在应该走的路
   (失败尝试里它被读成了 `ExpressionStat`)。
```

**为什么前两次失败在这里能解释清楚:** 两次都只动了 specifier 一侧(关掉 mark、
重开后 `continue`),而 ①② 之后的那段代码仍然在等"specifier 序列自己吃掉了类定义"
这个前提 —— 第 3 步没做,于是声明那一侧拿到的是一个光标停在 `;`、却没有任何节点
代表类定义的序列。第一次的报错是计数器没跟着走(已量出),第二次的 `ErrorNode` 坍塌
是第 3 步缺失的表现。



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
