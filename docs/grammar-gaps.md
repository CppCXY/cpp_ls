# 语法缺漏登记表

本文档记录**当前 parser 读不了的 C++ 构造**，以及每条的性质、成因和处置计划。

它存在的理由和 `crates/cpp_parser/tests/gaps.rs` 是同一个，只是回答的问题不同：那个文件回答"哪些构造**会**工作"，是回归护栏；这里回答"哪些**不**工作、为什么、打算怎么办"，是工作队列。构造一旦修好，就把它从这里删掉、写进 `gaps.rs` 的已支持清单。

**要开始干活，先读 [`roadmap.md`](roadmap.md)**：那一份是当前的队列（parser 与语义两条线、每条的最小复现与做法、一轮的配方），
本文档是它背后的规格——每条缺口的四要素，以及末尾三十六条**维护约定**。

## 现在的队列（打开着的条目）

本文档按批次编号，**未修好的**只剩这几条；其余条目是已修构造的规格与教训，按需要查，不必通读：

| 条目 | 是什么 | 值多少 |
|---|---|---|
| **B119** | `return 1 }`：语句末尾少一个 `;` 时**树是对的、报错是空的**（`parse_return_statement` 发的是零宽 `MissingNode`，而它**有意**不记 `CppParseError`）——语言服务器的诊断就是 `view.errors()`，所以用户少打一个分号时一个波浪线都没有 | 每条语句都要，且这是打字过程中最常见的中间状态 |
| **B122** | **`_TRY_IO_BEGIN` 这类"体是另一个宏的名字"的宏**没有解析链：`iosfwd:27` 说 `#define _TRY_IO_BEGIN _TRY_BEGIN`，而 `_TRY_BEGIN` 才是 `try {`（`yvals.h`）。于是语句位置的一次调用读不出来，`<xstring>` 在 524 行报 ``expected `;` after expression``，恢复时吃掉一个 `{`，**整个类体再也配不上**：`basic_string`（2444 行开始）本该在 5047 行 `};` 结束，实际吞到 5416 行——`std::string` 那五个别名（5300-5306）和 `erase`/`erase_if` 全落进 `std::basic_string` 里 | MSVC 的 `std::string` 全靠它（8 条查询里 5 条）；两步修法见下 |
| **B42** | 函数定义里的 `try`（function-try-block）：`void f() try { } catch (…) { }` | 语料里 0 个文件 |
| **B65** | 声明按分支各写一遍，每个分支自带尾巴和分号（第二支是**片段**） | 1 个文件（`parallel/algorithmfwd.h:700`） |
| **B72**（剩） | `STDMETHOD(QueryInterface) (…) PURE;`：声明符的名字在**宏自己的实参**里 | 1 个文件（`commdlg.h:577`） |
| **B77**（剩） | 初始化式里的指令：`parse_a_definition_per_branch` 的**最后那个 `;` 归谁**是设计问题 | 1 个文件（`ext/concurrence.h:58`，三版撤回） |
| 语料队列 | 其余失败文件的清单随时可重排：`%TEMP%\stdprobe\run_*.txt` 里"每个失败文件的首错"那一节 | 20 个文件（455 那份） |
| **宏族**（设计级） | 约一半首错行上有**闭包定义过、形态 Unknown** 的宏（`_GLIBCXX_NOEXCEPT_PARM`、`STDAPICALLTYPE`、`_CONST_RETURN`、`_GLIBCXX_MATH_NS`…）。修法不是又一条形状规则，而是把**位置化的宏证据**喂给 parser——`index-design.md` §"位置化的宏，第二个消费者"记着做法，parser 侧的 API 与测试已落地（B81），剩下每个文件 seeds 的计算与实测 | 约 8–10 个文件 |

驱动这一切的队列与逐批数字在 [`roadmap.md`](roadmap.md) §2.0；本文档是它背后的**规格**。
## 判定原则

本项目放弃上下文式解析，不等符号表。这决定了缺漏分两类，处置方式完全不同：

- **缺规则（missing rule）**——语法本身能判定，只是规则还没写。这类**都能修**，只是成本不同。
- **缺信息（missing information）**——从这段代码的 token 里无论怎么看都定不下来，需要跨编译单元的类型索引。这类是**刻意的取舍**，不是待办。

每条都会标注属于哪一类。标了"取舍"的不要试图消灭它，那会走回 clang 的老路。

### B119（新开的一条轴）：**该报错却没报**——`MissingNode` 是树里的话，不是客户端的话

这一条不是"读不了"，是"读对了但没说"。它是接语言服务器时暴露出来的第一件事（`cpp_ls` 的端到端测试
原来拿 `int g() { return 1 }` 当"一定有错"的素材，结果诊断列表是**空的**）：

```text
cpp_dump:  0 error
树:        ReturnStat@10..19 { return, LiteralExpr(1) } → Token(RightBrace)@19..20
编译器:    g++ 直接拒绝（expected `;` before `}`）
```

**成因**是 `parse_return_statement` 的最后一段：

```rust
if p.current_token() == CppTokenKind::Semicolon { p.bump(); } else { p.emit_missing_node(); }
```

`emit_missing_node` 的文档说得很清楚——零宽节点"让补全在坏掉的位置也能工作"——它是**树里的标记**；而
`get_errors()`（也就是 `FileView::errors()`，也就是客户端的诊断列表）**只收 `CppParseError`**。于是同一件事
在树里看得见、在协议上不存在。同一个文件里 `parse_expression_statement` 是**报错**的
（`expected `;` after expression`），`parse_keyword_statement` 也是——所以这不是设计，是**三处不一致**。

**它值多少**：用户把函数体写完但还没打最后那个分号时，今天客户端**什么都不显示**，打了分号才"突然好了"。
语言服务器的诊断全部来自这个列表，所以这一类（"恢复成功但没报"）有多少，用户就瞎多少。

**做法（便宜，但要一次决定）**：把"缺的是一个**终结符**"（语句末尾的 `;`、`)`、`}`）与"表达式读到一半"
分开——前者在**语句/声明的收尾处**报一条 `CppParseError`（范围取当前 token 的起点，与
`parse_keyword_statement` 同形），后者继续只发 `MissingNode`（那是"还在打字"，边界就在这里）。

**要注意的第二件事**：`CppParseError` 是**普查的口径**（干净/报错），所以这一改语料读数会变——变的是
"有多少文件会报一条我们以前不报的错"，**不是**"有多少文件读不下来"。改的时候两份数字必须分开说，
`std_probe` 的"干净/报错"与"每文件错误数直方图"要一起看，并在 §0 记下新的 455/128 读数。

## 严重度分级

| 级别 | 含义 | 为什么排这个顺序 |
|---|---|---|
| **A0 静默错树，且无任何标记** | 不报语法错误，树里**也没有** `ErrorNode`/`MissingNode`——只是节点类型是错的 | 最危险。上面三种护栏（无损、良构、报错）全都拦不住 |
| **A 静默错树** | 不报语法错误，只在树里留下 `ErrorNode`/`MissingNode` | 编辑器功能静默失效，用户拿不到任何提示，测试也不容易发现 |
| **B 报错拒收，成本低** | 报错，但规则本身简单 | 性价比最高，改动局部 |
| **C 报错拒收，成本高** | 报错，且需要成体系的子语法 | 要单独排期 |
| **D 非标准扩展** | GNU/MSVC 方言 | 建议长期搁置 |

---

## A0 类：静默错树，且无任何标记

这一类是本表最初的盲区，补记在这里。它的危险不在于"没报错"——A 类也没报错——而在于**树里连一个 `ErrorNode` 都没有**：整棵树形状合法、无损，只是描述的构造是错的。

`gaps.rs` 原来的判据是 `errors ∪ ErrorNode ∪ MissingNode`，三种情况全空；`invariants.rs` 验的是无损与良构，而错树同样无损、同样良构；作用域层则会为"没名字的声明符"收手，于是错树与正树产出**同一个（空）名字集合**。所以四种护栏一条都拦不住——它是靠手工抽查发现的。

**处置不是补一条规则，而是补一个问题**：`gaps.rs` 新增 `constructs_are_read_as_the_right_node`，通过 `Shape` 断言"这个构造必须读成这种节点"。判据从"干净吗"变成"读对了吗"。

### A0-1. 赋值被读成声明 —— 已修复

```cpp
void f() { x = 1; }        // 0 报错、0 ErrorNode，但读成了 Declaration
```

**现象**：读成

```
Syntax(Declaration)
  Syntax(DeclSpecifierSeq)   x
  Syntax(InitDeclarator)
    Token(Assign) "="
    Syntax(Initializer)      1
```

——一个**声明符没命名任何东西**的变量声明。凡是左值在本文件里没有类型记录的赋值都这样，也就是函数体里的大多数赋值。

**性质**：缺规则。**成因**：`x` 未出现过，于是 `x = 1;` 走回落路径；`parse_declarator_with` 在 `=` 上返回**空声明符**，而 `finish_init_declarator` 无条件接受 `=`。**没有任何地方要求"有初始化器就必须有声明符名"。**

**修复**（`decls.rs`）：新增 `an_initializer_needs_a_name`，在 `finish_init_declarator` 消费掉声明符之后的属性、进入 `match` 之前拒绝：

```rust
matches!(p.current_token(), CppTokenKind::Assign)
    && !a_name_was_parsed(p, declarator_from)
    && !p.has_qualified_declaration_type_name()
    && !declarator_starts_with_a_type_keyword(p)
    && !declarator_starts_with_a_known_type_name(p)
```

**为什么不是简单的一句"没解析到名字"**：**限定声明符**同样"没解析到名字"——`int ns::count = 0;` 里 `ns::count` 是被 specifier 序列吃掉的，声明符规则什么都没拿到。这条一直是好的，而且是 C++ 的正当读法（`count` 是静态成员）。区分二者的是"前面那串**写成了类型**吗"：限定名至少两段，裸的未声明 `x` 只有一段。`has_qualified_declaration_type_name` 正是 specifier 序列走 `::` 段时置的那个标志。

**顺带暴露的一个用例**：`x = 1, y = 2;` 过去靠这条错树"过"了；现在它响亮地报 B4（逗号运算符缺失）。这是改善——从静默错树变成已知缺口。

**护栏**：`gaps.rs::constructs_are_read_as_the_right_node` 钉住 18 条语句的节点类型；`direct_init.rs::an_assignment_is_not_a_declaration` 与 `a_qualified_declarator_keeps_its_reading` 钉住这条门的两侧。

### A0-2. 无名参数的函数声明被读成变量 —— 已修复

```cpp
void f(T);                              // 0 报错、0 ErrorNode，读成"变量 f 用值 T 直接初始化"
template <typename T> void f(T) { }     // 报错：类体的 `{` 没地方放
template <typename T> void f(T) requires C<T>;   // 同上
```

**现象**：`void f(T);` 读成

```
Syntax(Declaration)
  Syntax(DeclSpecifierSeq)   void
  Syntax(InitDeclarator)
    Syntax(Declarator)
      Syntax(NameExpr)       f
      Syntax(Initializer)
        Syntax(ArgumentList) (T)
```

——一个**直接初始化的变量**，而不是"一个无名形参的函数"。只有 `void f(T);`（声明）是静默的；带函数体时 `{` 没有声明可归，于是响亮报错。

**性质**：缺规则（判据不够细）。**成因**：`T x(...)` 的两种读法——直接初始化与形参列表——token 完全相同，只能靠判据。`parse_function_suffix_or_initializer` 里有一条**文件作用域优先读初始化器**的规则，为的是 `Max(a, b);`：那里**根本没有类型**（`Max` 是被当成类型的那个名字），而文件作用域不存在调用语句，所以优先当声明。问题在于这条规则问的是"括号里是不是一串裸名字"，`(T)` 满足，于是 `void f(T);` 也走了这条路——**尽管它的声明已经有一个明确的类型关键字 `void`**。

**修复**：新增 `a_type_keyword_precedes_the_declarator_name`——从 `(` 往前，跨过声明符自己的名字（限定名 `A::f` 也整个跨过），看紧挨着的那个 token 是不是类型关键字。是，就说明这条声明**有类型**，括号只能是形参列表，初始化器优先那条规则不适用（它本来是为"没有类型的声明"写的）。

**为什么不能复用旁边那个同名的判据**：`declarator_starts_with_a_type_keyword` 找的是**整条声明的第一个** token，而模板头把它推远了——`template <typename T> void f(T)` 的第一个 token 是 `template`，不是类型关键字，于是这条最该被修的写法恰好落在判据之外。新的判据从声明符自己的名字起往前看，模板头在它身后，类型在它身前。

**顺带的一致性**：`int a(b);` 在**块作用域**里一直读成形参列表（`a_parameter_list_is_not_a_value_list` 早就钉住了它），而文件作用域读成变量——同一串 token 两种读法。修完之后两处一致，也与 C++ 在 `b` 是类型时给的答案一致。

**仍留着的一条（响亮，已登记下条 B14）**：**限定名 + 无名裸类型形参**。

**护栏**：`direct_init.rs::an_unnamed_parameter_of_an_unknown_type_is_a_parameter`（9 条必须读成形参列表的写法、4 条必须保持原读法的写法、1 条已知缺口的报错断言）；`gaps.rs` 已支持清单 5 条、形状断言 3 条（`Parameter` 的**计数**是关键——错误读法下它是 0）。

### A0-4. 类型后面的 cv 限定符把声明符的名字吃进类型 —— 已修复

```cpp
char const w[] = { 'a' };   // 0 报错、0 ErrorNode —— 类型被读成 `char const w`，声明符成了 `[]`
int const x = 1;            // 同上：类型 `int const x`，声明符空
char const w[2] = { 'a' };  // 报错（同一个读法，只是下标让它露出来）
char const* p = 0;          // 一直读得出 —— `*` 在名字之前就把说明符序列结束了
```

**现象**：`char const w[] = { 'a' };` 读成"类型 `char const w` + 一个**空的**结构化绑定 `[]`"——良构、无损、**零报错**。`w` 这个名字从未进入声明符，所以作用域层拿到的名字集合是错的（少一个 `w`，多一个空绑定）。`char const w[2]` 是同一个读法撞上数字下标，才响亮报错。

**性质**：缺规则，而且是**两个缺陷叠在一起**——第二个遮住了第一个，所以修一个不够：

1. **`type_is_already_complete` 问错了对象**：它判断名字**紧邻的前一个 token**，而 cv 限定符给出的答案是"类型还没完"。但限定符既不完成一个类型、也不*取消*一个类型——`const char w` 与 `char const w` 是同一个类型。现在它**跨过限定符**，去判断限定符前面的东西（`char const w` 看到 `char` → 类型已完成）。
2. **`has_type_specifier` 是"赋值"而不是"累积"**：这个标志的语义是"**这个说明符序列里出现过类型**"，但每次说明符跑完都**整体覆盖**它。cv 限定符不命名类型，于是把它清成了 false —— 到了 `w` 那里，`name_joins_the_type` 的第一句就是"还没有类型，那这个名字只能是类型"。

**只修第一处的后果**：`type_is_already_complete` 答"已完成"了，但 `has_type_specifier` 仍然说"还没有类型"，判据在**更早的一句**上返回——同一个错树，换条路走。这正是本文档反复出现的"修好一处，另一处还在"：**当一个标志的语义是"序列里出现过 X"，它就必须累积**。

**为什么一直没被发现**：最常见的写法 `char const* p` 不受影响——`*` 在名字之前就把说明符序列结束了。要露出来必须"限定符后面直接跟名字"，也就是 `char const w`、`int const x` 这种**把 const 写在类型后面**的写法：C 里常见（`char const *` 是同一件事的另一种写法），而在本项目的语料里恰好没有。

**发现经过**：追 B23 的 CMake 生成文件时撞上的。那份文件里 `char const info_version[] = { ... };` 是**主要错误来源**：修掉这两处之后，`CMakeCCompilerId.c` 从 10 条报错降到 4 条。**又一次印证维护约定第 11 条**——真实文件（哪怕是生成的文件）会撞出手写清单想都想不到的形状。

**护栏**：`direct_init.rs::a_cv_qualifier_after_the_type_does_not_swallow_the_name`（12 条必须读成"一个 InitDeclarator、零个 StructuredBinding"，外加 `ArrayType`/`Declarator` 的形状断言）；`gaps.rs` 已支持清单 5 条 + 形状断言 2 条。

### A0-3. requires-clause 写在类头上时类体被丢到文件作用域 —— 已修复（见 C1）

`template <typename T> struct S requires C<T> { };` 曾经读成"结构体声明 + 文件作用域上的一个 `CompoundStat`"：类体不再属于类。它同时是**非标准写法**（标准里类头没有 requires-clause 的位置），所以修法是**拒收**而不是补规则——详见 C1 条目。

---

## A 类：静默错树

这几条**不报错**，只在树里留下 ErrorNode。测试最容易漏掉，用户最难定位。

### A1. 转换运算符（conversion operator）—— 已修复

```cpp
struct S {
    operator int();              // 4 个 ErrorNode，0 报错
    operator const char*();      // 4 个 ErrorNode，0 报错
    operator std::string();      // 1 个 ErrorNode，0 报错
    operator bool();             // 4 个 ErrorNode，0 报错
};
```

**性质**：缺规则。**成因**：`types.rs::parse_one_decl_specifier_inner` 只在 `specifier_seen` 为真时才接受 `operator`（第 535 行），那条路是给 `explicit operator bool()` 留的——`explicit` 先作为 specifier 被吃掉，`operator` 才有上下文。**裸** `operator int()` 的第一个 token 就是 `operator`，specifier 循环直接拒绝，声明读法失败。

失败之后是最坏的一种失败：token 被拆成碎片硬塞进树里——`operator` 成一个 ErrorNode，`int` 被当成一个独立声明（顺带把 `int` 记进了本文件的类型表），`()` 再成一个。所以既不报错、树也完全错位。

**修复**（`decls.rs`）：在 `parse_declaration` 顶部对首 token 为 `OperatorKeyword` 的情况直接派发到 `parse_conversion_operator_declaration`——与析构函数同构，理由也相同：名字写在最前面，前面没有类型。安全的前提是**没有别的声明以 `operator` 开头**（重载运算符的返回类型在前，`Ops operator+(const Ops&)`），所以这个派发不需要额外条件。

`parse_operator_name` 的转换运算符分支补齐了两个洞：`Identifier | Scope`（`operator MyType`、`operator std::string`）和入口守卫里的 `const` / `volatile`（`operator const char*`）——后者最初只加在循环里没加在守卫里，症状是 `operator char()` 能过而 `operator const char()` 不能。

**期间发现并修掉的两件事**：
- 引用限定符 `operator bool() &&` 与右值引用类型 `operator T&&()` 共用 `&&`。判据是**源码相邻性**：`T&&(` 贴在一起是类型，`() &&` 分开是限定符（`types.rs::a_ref_qualifier_is_here`，新增 `CppParser::peek_token_range_at`）。
- `parse_declaration` 里模板头之后直接调 `parse_using_declaration`（而非递归 `parse_declaration`），否则多开一层 `Declaration` marker，harness 断言 `a grammar rule unwound past the translation unit marker` 会 panic。

### A2. 成员位置的 `alignas` —— 已修复

```cpp
struct S { alignas(16) int x; };     // 4 个 ErrorNode，0 报错
```

**性质**：缺规则。**成因**：`alignas` 不是 decl-specifier，specifier 循环没有分支，成员规则把它当表达式读。

**修复**（`types.rs`）：新增 `CppSyntaxKind::AlignasSpec`，specifier 循环里加 `AlignasKeyword` 分支，载荷用 `parse_type_id_or_expression` 读（`alignas(16)` 是常量表达式、`alignas(int)` 是类型、`alignas(alignof(int))` 又是表达式——与 `sizeof(...)` 是同一个歧义，所以共用同一条规则）。

**但真正的工作量在第三件事**：specifier 循环的「见过 specifier 了吗」这个标志必须拆成「见过**类型**了吗」。`alignas(16) MyType value;` 消费了一个 specifier 而**没有**命名类型，旧标志让 `MyType` 被读成完整类型的第二个名字，`value` 就没地方当声明符了。见下文。

**过程中的教训**：我先用「回退扫描时跳过 alignas 载荷」的办法打补丁，改了四轮，每一轮都只修好一部分——`struct E { }; alignas(16) E e;` 好了，`alignas(16) alignas(32) E e;` 又坏了。根因是那个扫描本身就不该被问这个问题。**当补丁开始需要第二个补丁时，该换的是问题而不是答案。** 换成 `has_type_specifier` 之后，回退扫描里所有 alignas 相关代码都删掉了，一行不剩。

### A3. C++23 显式对象参数 —— 已修复

```cpp
struct S { void f(this S& self); };   // 6 个 ErrorNode，0 报错
```

**性质**：缺规则，且**属于 A0 类**。原先记为"成本中等、优先级低"，那个判断低估了它：它**不报错**，而是把 `void f(this S& self);` 读成"什么都没有"，同时把 `this` 之后的 token 读成**第二个、幻影成员**。所以 `gaps.rs` 看不见它——它是靠一次"常见构造普查"发现的，不是靠测试。

**成因**：`this` 在参数位置是**类型**（对象的推导类型），而别处它都是表达式，所以 specifier 序列读不了它；参数列表在 `this` 处放弃，声明读法失败，后面的 token 就归了下一条声明。

**修复**（`decls.rs::parse_parameter`）：`this` 在参数位置单独成一条规则——读一个 `ThisExpr` 节点，然后**整体读一个 type-id**。这覆盖 `this S&`、`this S&&`、`this auto&&`、`this S*` 而不需要每种一个分支；类型之后的声明符名与默认实参走通用规则。

**为什么不是"只读后缀"**：第一版把 `this` 之后当作声明符后缀读（`*`/`&`/`&&`），于是 `S` 被丢下——**后缀不是类型**。`this` 后面写的是类型，交给类型规则才是对的。

**为什么不需要额外守卫**：`f(this);` 里 `this` 之后不是类型，`parse_type_id` 自然拒绝，调用方回退到表达式读法。`this` 作实参因此不受影响。

**护栏**：`gaps.rs::modern_constructs_produce_the_right_nodes` 断言**恰好一个 `Parameter`**——静默版本在这里是 0，同时旁边多一个幻影成员；`modern.rs` 另有一组 `this` 作实参的反例。

---

## B 类：报错拒收，成本低

### B8. 替代运算符记号 —— 已修复

```cpp
auto x = a and b;      // expected primary expression
auto x = not a;        // 同上
auto x = a bitand b;   // 同上
```

**性质**：缺规则。这些是 `<iso646.h>` / 标准自己的字母拼写，**是标准 C++ 而不是扩展**，所以忽略它们等于拒收能编译的代码。

**成因**：**词法器把它们当标识符**（`and`、`or`、`not`… 不在关键字表里），而运算符表按 token kind 查——于是 `and` 永远匹配不上 `&&`。有意思的是 `not_eq(a, b)` *看起来*能过：那是因为 `not_eq` 被当成函数名读成了调用。

**修复**（`exprs.rs::the_alternative_operator`）：按**文本**映射，在需要用它的两处调用——二元运算符表（`get_operator_precedence`，所有调用方都经过它）与一元/主表达式规则（`not`、`compl` 必须先于"读一个名字"被认出来）。

**为什么不改写 token 流**：改写是一处改动而不是三处，但它同时改掉了 token **是什么**——`and` 会自称 `&&`，而打印运算符的消费者会打印出文件里根本没有的 token。文本两种做法都保留，要对的只是形状。

**顺带**：`is_fold_operator` 从"再列一遍运算符"改成从运算符表推导（`precedence > 1`），于是 `a and ... ` 这类折叠无需另行提及，两张表也不会再有机会互相矛盾。

**一个不算缺口的条目**：`a compl b` 不读——但 `compl` 在 C++ 里**只有一元形式**，`a compl b` 本身就不合法，所以这不是缺口。

### B9. `throw` 表达式 —— 已修复

```cpp
x = throw 1;                 // expected primary expression
auto y = cond ? throw 1 : 2; // 同上
return throw 1;              // 同上
```

**性质**：缺规则，而且是**同一个关键字的另一半**：`throw e;` 作为语句早就有规则（产出 `ThrowStat`），但 `throw` 出现在需要**值**的位置时没有任何表达式规则。

**成因**：`ThrowKeyword` 在 `is_expression_keyword` 里——那张表是一张**承诺**（"有规则会消费这个 token"）——而没有规则消费它。这与 `co_await` 当初缺失是同一类问题，文档在 C1 里已把它记为"空头支票"。

**修复**（`exprs.rs::parse_unary_expr` 的 `ThrowKeyword` 分支）：新增节点 `ThrowExpr`，操作数**可选**——是否读取由 `throw_has_no_operand` 判断（后随 `;`、`)`、`]`、`}`、`,`、`:` 或文件尾即无操作数），这正是 `throw;` 这一 rethrow 拼写所需要的。

**两者都保留**：语句形式产出 `ThrowStat`（消费者在那里要的就是它），表达式形式产出 `ThrowExpr`，形状断言把两边都钉住，包括"`x = throw 1;` 里不得出现 `ThrowStat`"。

### B10. `extern template` 与 `inline namespace` —— 已修复

```cpp
extern template struct S<int>;   // expected `;`
inline namespace v1 { }          // expected primary expression
```

**性质**：缺规则。两个都是**关键字对**，而每一对里没有一个关键字是类型。

**成因**：`extern template` 不是 linkage specification（那个后面跟字符串字面量），`extern` 被 specifier 序列吃掉后停在 `template`；`inline namespace` 里 `inline` 被吃掉后停在 `namespace`，而 `namespace` 不是类型。

**修复**（`decls.rs::parse_declaration`）：
- `inline` + `namespace` 在 dispatch 处消费 `inline` 后转交 `parse_namespace_declaration`；
- `extern template` 在 **`Declaration` 节点之内**消费两个关键字——token 在消费的那一刻落位，在 marker 之前消费会让 `extern template` 落在声明**旁边**而不是**里面**，而"这是实例化声明还是定义"正是靠读声明自己的 token 回答的。这也是 `export` 那次踩过的同一个坑。

`extern template` 只在 `template` 之后确实跟得下一个类型说明符时才吃，否则把 `extern` 留给 specifier 序列，让错误信息停在正确的位置。

---

## B 类：报错拒收，成本低

### B1. `alignas` —— 已修复

见 A2。两处是同一件事，`alignas` 加进 `starts_declaration` 的锚点列表：

```rust
CppTokenKind::AlignasKeyword => true,
```

那个锚点不是优化。没有它，`alignas(16) E e;`（类型是**未限定名**）会走「试着读声明、失败、改读表达式」的弯路，最后在 `alignas` 上报 `expected primary expression`——一个 specifier 循环刚刚学会读的 token。带类型的名字（`alignas(16) int a;`）不走那条路，所以缺口只在未限定名下露出来。

### B2. 别名声明的数组/函数类型 —— 已修复

```cpp
using T = int[4];                    // expected `;` @13..14
using T = int();                     // expected `;` @13..14
typedef int Arr[4];                  // 一直是好的
```

**性质**：缺规则。**成因**：`using` 别名右侧走 `parse_type_id`，而 type-id 的抽象声明符只读指针/引用/限定符，**不读后缀**（数组/函数）——因为其他所有出现 type-id 的地方，后缀都属于外层构造。别名里没有外层：`=` 之前是类型，`;` 之后什么都没有。

**修复**（`decls.rs::parse_using_declaration`）：读完 type-id 后，若游标在 `[` 或 `(`，继续调 `types::parse_declarator_suffixes`。这个不对称正是 `typedef int Arr[4];` 一直能过而 `using` 不能的原因。

### B3. 属性位置 —— 已修复

```cpp
int x [[maybe_unused]] = 1;                  // 声明符之后
void f() [[noreturn]];                       // 函数声明符与 `;` 之间
using T [[deprecated]] = int;                // 别名声明
enum class E { A [[deprecated]] = 1 };       // 枚举项
template <typename T> [[nodiscard]] T p();   // 模板头与其声明之间
void f(int x [[maybe_unused]]);              // 参数
void f([[maybe_unused]] int x);              // 参数（类型之前）
```

**性质**：缺规则——`parse_attribute_specifier` 早已存在且完善，缺的只是**调用点**。

**修复**：新增 `types::parse_attribute_specifiers`（读一串 `[[...]]`，没东西时静默返回），在五个位置调用：`finish_init_declarator`（声明符之后、初始化器之前）、`parse_parameter`、`parse_using_declaration`、`parse_enumerator_body`、以及 `parse_declaration` 里模板头之后。

**两个坑**：
- 声明符的**后缀循环**把 `[[` 读成了数组下标（`[` + `[maybe_unused`），导致整个声明被拒。后缀循环里加了一条：`[[` 就 break。
- 模板头之后那个位置原本既不属于头也不属于 specifier 序列（头已经闭合、序列还没开始），所以落到了声明符手里报 `expected ';'`。

**仍未支持**：`namespace [[deprecated]] n { }`——`namespace` 和名字之间的属性。那里既没有声明符也没有 specifier 序列可挂。已记在 `tests/ast.rs::known_unparsed_attribute_positions`。

### B4. 括号里的逗号运算符 —— 已修复

```cpp
auto x = (a, b);                     // 已修复
x = 1, y = 2;                        // 已修复
return 1, 2;                         // 已修复
for (a = 0, b = 0; ; ) {}            // 已修复
```

**性质**：缺规则。**成因**：运算符表里没有逗号，而它**不能**加进去——`get_operator_precedence` 被 `parse_binary_expr_with_precedence` 查，而**每个列表规则都通过它读元素**。逗号进了那张表，`f(a, b)` 就变成"一个实参"、`{1, 2}` 就变成"一个元素"，而容器**没有办法退出**。

**修复**（`exprs.rs`）：把逗号放在**列表之上**，而不是运算符表里。

1. 把「读一个表达式」拆成两级入口：`parse_expr`（含逗号）与新增的 `parse_assignment_expr`（到赋值族为止，即"一个元素"）。两者共用 `parse_expr_up_to(p, level, pack_expansion)`。
2. 逗号在 `Level::Full` 那一层左折叠成 `BinaryExpr`，右操作数是**三元**表达式而非另一个逗号表达式——这就是左结合，`a, b, c` 折成 `(a, b), c`。
3. **所有"自己拼逗号序列"的规则改调 `parse_assignment_expr`**。

**第 3 条是本条目的真正成本，也是维护约定第 5/6 条要的那份清单。** 它现在写在 `exprs.rs` 的 `Level` 文档里，含"为什么不在清单上"的部分：

| 规则 | 它分隔的序列 |
|---|---|
| `parse_postfix_suffixes`（调用分支） | 实参 |
| `decls::parse_expression_list` | 括号初始化器、基类列表、成员初始化器列表 |
| `decls::parse_initializer_clause` | 花括号初始化列表的元素 |
| `exprs::parse_capture` | lambda init-capture 的初始化器 |
| `decls::finish_init_declarator`（位域分支） | 位域宽度 |

**最后一行是踩出来的**：实现完前四处后 `cargo test` 报 `unsigned flags : 1, spare : 7;` 多出 ErrorNode——位域宽度把第二个字段吃了。位域**不是**通常意义的表达式列表，但**成员**是逗号分隔的。这条正是"先写清单再改"的理由：清单里漏掉的，测试会替你发现，但发现得晚。

**顺带发现的一件事**：pack expansion 也必须留在**元素**那一级。`g(args...)` 是"实参表里的一个 pack 展开"，所以元素读取器若在 `...` 前停下，`...` 就留给列表去绊倒——第一版正是这么写的，`g(args...)` 立刻坏了。`parse_expr_up_to` 的 `pack_expansion` 参数因此在**两级**都生效。

### B5. `void()` 作为表达式

```cpp
return void();                       // expected primary expression
```

**性质**：缺规则。**成因**：`void` 是类型关键字，不是表达式关键字，`return` 的表达式规则不认。**处置**：便宜，但很少见。优先级低。

### B11. `decltype` 作类型说明符 —— 已修复

```cpp
decltype(x) y;             // 一直读得出
decltype(x) y = 1;         // 曾报 expected primary expression
decltype(auto) x = f();    // 同上
decltype(x) v[2];          // 同上
using T = decltype(x);     // 一直读得出
decltype(x)* p;            // 一直读得出
```

**性质**：缺规则，而且是**两个缺陷叠在一起**，两个都不在错误信息指向的地方。

**症状骗人**：没有初始化器就正常、加上就失败，看起来像"初始化器"的问题。**不是。**

**缺陷一**：`decltype` 不在 `starts_declaration` 的锚点表里，所以声明读法**根本不会被尝试**——语句层把它当名字，随后撞上 `y`，报 `expected primary expression` 指向 `decltype` 自己。

**缺陷二**（修好第一个之后才露出来）：specifier 循环通过**"看最后一个 token"** 来回答"这个说明符命名了一个类型吗"：

```rust
*has_type_specifier = p.last_consumed_token_kind().is_some_and(|kind| { … });
```

而 `decltype(a)` 的最后一个 token 是它载荷的 `)`——**正是 `alignas(16)` 结尾的那个 token，也正是这个判据存在的目的所要回答"否"的那个**。于是类型被判为"未完成"，`name_joins_the_type` 第一句就返回"这个名字只能是类型"，**把声明符自己的名字吃进了类型**：声明出来时没有声明符，声明读法失败，语句回退成表达式。

**修复**：
1. `decls.rs::a_decltype_here_is_a_type` —— `decltype` 加进锚点判据，条件是"试读 type-id 之后紧跟声明符起点"，于是 `decltype(x);`、`decltype(x) + 1;` 这类**表达式**仍走表达式读法；
2. `types.rs::parse_one_decl_specifier` —— "命名了类型吗"改为**读规则自己产出的节点**：记录进入前的 `events_len`，若该说明符产出了 `BuiltinType` 即为真。`BuiltinType` 只由真正命名关键字类型的分支产出，而"某个分支忘了设标志"正是这个函数存在的理由——所以记录从**规则实际产出的东西**读回来，而不是再添一个标志。

**载荷是类型的那一个例外**：`decltype(auto)` 里是类型而非表达式，而 `auto` 是任何表达式规则都不接受的 keyword，所以载荷读法加了一个分支：游标在 `auto` 时读 type-id，否则读表达式。区分依据正是那个不可能是表达式的 token。

**为什么这比"放松 A0-1 守卫"对**：放松守卫会让 `decltype(x) y = 1;` 过——但那是**用一个错树换一个拒收**。真正的修法是让声明符的**名字**归声明符，而这也是它本来就该在的地方。

**护栏**：`modern.rs::a_decltype_declaration_has_a_declarator` 断言**恰好一个 `InitDeclarator` 与一个 `Declarator`**——缺陷版本在这些位置是 0，而"能解析吗"这个问法看不见它。

### B12. `requires` / `concept` 作标识符 —— 已修复

```cpp
int requires = 1;            // expected a type specifier
void f() { requires = 1; }   // 同上
int concept = 2;             // 同上
void f() { concept(); }      // 同上
```

**性质**：缺规则，但在**词法层**。**成因**：`cpp_lexer.rs` 把 `requires` 和 `concept` 登记成了关键字，于是它们永远到不了"这是个名字"的分支。它们其实是**上下文关键字**——`final`、`override`、`module`、`import` 都是按标识符进入 parser 再按文本判定的，只有这两个没有。

**修复**：把这两个词从词法器的关键字表里删掉，并把 `CppTokenKind::RequiresKeyword` / `ConceptKeyword` 两个 token 种类**整个删除**（留着就是留一个没人该产生的值）。语法层新增 `grammar/cpp/mod.rs::is_contextual_keyword`、`at_requires`、`at_concept`、`expect_contextual_keyword`，8 处判定改成按拼写；`modules.rs` 里那份私有的 `is_contextual` 改为委托同一实现——`module`、`import`、`final`、`override`、`requires`、`concept` 是**一家人**，一个判据只能有一份。

**做的时候才看清的三件事**（每一件都是"词是标识符"直接推出来的）：

1. **`concept` 不能当 `starts_declaration` 的锚点。** 第一版加了 `Identifier if at_concept(p) => true`，结果 `void f() { concept = 2; }` 被导去声明读法，在 `=` 上报 `expected a concept name`——锚点的意思是"这里一定是声明"，而 `concept = 2;` 恰恰是**不是**声明的那个反例。真正的判据是"**模板头 + 这个词**"：concept 定义一定有头，而头是 parser 刚刚读过的信息，不需要另查。删掉锚点、给派发加上 `seen_a_template_head &&` 之后，两种读法各归各位。
2. **嵌套 requirement 要判一次"读得下去吗"。** `requires C<T>;` 是 clause，`requires;` 是名为 `requires` 的**简单 requirement**——同一个词，后者是名字。判据用的是 C1 里那条试读（`starts_a_requires_clause`），因此把它从模块私有提到 `pub(super)`，让表达式语法共用。这与 requires-expression 那边早已存在的 `starts_a_requires_expression`（区分 `requires { }` 与 `requires(x)` 这个调用）是同一条思路。
3. **`is_expression_keyword` 的那一条要删，而不是留。** 那张表的语义是"有别的规则消费这个 token，名字分支请让开"。词变成标识符之后，消费它的是 `Identifier if at_requires(...)` 分支，名字分支是兜底——**这张表本来就不该收它**。空头支票（C1 里记的那张）的结局有两种：兑现，或者发现这张表从一开始就不该有它。这次是后者。

**顺带的一致性收益**：`requires`/`concept` 现在与 `final`/`override`/`module`/`import` 走同一条路，于是"哪些词是上下文关键字"这件事在代码里只有一个答案——词法器只放真关键字，语法层按拼写判定。

**护栏**：`concepts.rs::the_two_words_are_ordinary_names_everywhere_else`（16 条名字用法必须无错、无 concept/clause/requires-expression 节点、且读成 ExpressionStat / CallExpr / Declaration 的形状；再加反向的 3 条断言证明构造仍然读得出，以及 `requires;` 是 requirement 而非 clause）；`invariants.rs` 的 "requires and concept as ordinary names"；`gaps.rs` 已支持清单 5 条 + 形状断言 4 条；语料库新增一段名字用法。

### B13. 不带 `extern` 的显式实例化 —— 已修复

```cpp
template void f<int>(int);          // expected <, but get void
template int v<int>;                // 同上
template class C<int>;              // 同上
```

**性质**：缺规则。**成因**：`template` 后面直接跟类型说明符时，`parse_template_head` 要求 `<`，于是整个声明被回退。带 `extern` 的那半（`extern template …`）已经能读——B10 修的就是它，而两条路走的是**不同的代码**：`extern template` 在声明规则里就地消费两个关键字，裸 `template` 则先撞上模板头。

**修复**：两种拼法在标准里是**同一条产生式**——`explicit-instantiation: extern(opt) template declaration`——所以修法也应该是同一处：既然 `extern template` 的修法是"就地消费两个关键字，然后让普通声明规则跑"，裸 `template` 就该是"就地消费一个关键字，然后让普通声明规则跑"。区分二者的只有 `<`：

```text
template < … >   模板头，交给模板头规则
template 其它    显式实例化，消费掉 `template`，走普通声明
```

判据就是一次 `peek_next_token() != Less`——模板参数列表不可省略，所以 `template` 后面不是 `<` 就一定不是头。

**顺带修好的一处过窄判据**：`extern template` 原先还要求"`template` 后面那个 token 是**关键字**类型"，理由是"否则吃掉关键字会让后面没有东西当说明符"。这条理由是错的，而且错得没有症状：`extern template MyType f<int>(int);`（返回类型由本文件命名，完全合法）一直被拒。`extern template` 这两个词放在一起没有第二种读法，判据删掉即可，现在两种拼法共用一套处理。

**与本条同一族、一起修掉的显式特化**：`template <> void f<int>(int);` 与 `template <> int v<int>;`。空头也是头，所以它们走模板头那条路；但它们引入的声明同样**以 template-id 为名字**，于是被我自己在 C1 里加的那条"声明符的名字不能是裸 template-id"的规则挡住——那条规则是为"名字还是类型"的歧义写的，而特化**必须**说明是哪个特化，没有歧义可判。修法是在读到空头（`template` `<` `>`）时把同一个标志置起。这一条是**回归**：规则加入之前它是能读的。

**护栏**：`modern.rs::an_explicit_instantiation_without_extern_parses`（12 条：函数名、类头、变量名三种位置，外加与模板自身声明并排）、`modern.rs::a_template_head_is_not_an_explicit_instantiation`（14 条：模板头、别名模板、变量模板、concept、受约束函数、部分特化、显式特化都必须保持原读法，并断言显式实例化**没有** `TemplateDecl` 节点）；`gaps.rs` 已支持清单 9 条。

### B14. 限定名 + 无名裸类型形参 —— 已修复

```cpp
void Widget::draw(T) { }        // 曾报 expected primary expression
static void Widget::draw(T) { } // 同上（还叠着 B15）
void Widget::draw(int) { }      // 一直读得出
void Widget::draw(Canvas&) { }  // 一直读得出
void Widget::f(T t) { }         // 一直读得出
```

**性质**：缺规则（与 A0-2 同源：都是"括号该读成什么"的判据不够）。**成因**：限定名 `Widget::draw` 被 specifier 序列整段当成类型吃掉，声明符**自己没有名字**；而括号偏偏是 `(T)` —— **一串裸名字**，正是直接初始化与形参列表**唯一共享**的形状。于是那条"裸名字列表优先读初始化器"的偏好（它是为 `Max(a, b);` 写的）赢了，`(T)` 成了初始化器，接着函数体的 `{` 无处可去。

**修复**：判据是**"这条声明头上有 `::`，且这个声明符什么都没命名"**——那么它只能是**定义的头部**，括号只能是形参列表，没有第二种读法。在 `parse_function_suffix_or_initializer` 的最前面加这一条（形参读法先试，失败照旧回退，`ns::C::method(1, 2);` 这种调用不受影响）。

**与 B15 是同一条判据的两半**：B14 缺的是"知道头上是限定名"，B15 缺的是"知道头上有类型"。两半一起修，见下。

**处置**：半天。级别 B（报错，无静默风险）。

### B15. 前导存储说明符 + 限定声明符名里的 template-id —— 已修复

```cpp
void A::f<int>(int);            // 一直读得出
static void A::f<int>(int);     // 曾报 expected `;`
inline / extern / constexpr     // 同上
static void A<int>::f<int>(int);// 同上
template void A::f<int>(int);   // 同上（B13 修好之后才走到这里）
```

**性质**：缺规则（判据问错了对象）。**成因**：限定名 `A::f<int>` 被 specifier 序列整段吃掉（和 B14 同一个机制），声明符**自己没有名字**，于是它的后缀（那个 `(int)`）要不要读，取决于 `parse_declarator_with` 里那道门：`named || a_qualified_name_is_the_type(p) || a_declaration_is_the_better_reading(p, …)`。三个都为假时后缀根本不被读，`(int)` 留在原地，声明报 `expected ;`。

而那三个判据都靠**往回走找声明的第一个 token**：`declarator_starts_with_a_type_keyword` 一路退到 `;`/`{`/`}` 之前，取**最早**的那个显著 token。`void A::f<int>(int);` 退到 `void` → 类型关键字 → 门开；`static void A::f<int>(int);` 退到 `static` → 门关。**一个存储说明符就足以把类型关键字从判据的视野里挤出去**。

调试时还发现第二层原因，值得单独记：**`a_qualified_name_is_the_type` 在这种情况下本来就是假**。它要的是 specifier 序列记下来的 `declaration_type_name`，而那个记录是**往回走找一个 Identifier**——`A::f<int>` 的最后一个 token 是 `>`，走不到名字，记下来的是**空**。所以门的两条限定名判据在这条形状上**同时失效**，只剩"第一个 token 是类型关键字"这一个偶然成立的理由。

**修复**：新增 `the_head_of_the_declaration_is_qualified(p)` —— **问整条声明的头部，而不是它的第一个 token**：往回走到声明的开头（遇 `;`/`{`/`}` 停），路上有没有 `::`。这是**看 token**，不依赖任何记录下来的状态，所以 `A::f<int>` 这种"记录为空"的名字也能判对。它同时接进那道门（B15）和形参优先那条规则（B14）。

**方法论**：这是本文档里第 N 次出现同一个错误形状——**"第一个 token 是什么"被当成了"这条声明是什么"**（A0-2 的 `declarator_starts_with_a_type_keyword` 已经栽过一次，见维护约定第 9 条）。这次的修法是把它换成"**整条声明的形状里有没有某个东西**"，而这类问题在 token 里是可以回答的。

**护栏**：`direct_init.rs::a_qualified_declarator_behind_a_storage_specifier_is_still_a_definition`（10 条 + 形参列表计数 + template-id 仍在类型里）、`direct_init.rs::an_unnamed_parameter_of_an_unknown_type_is_a_parameter` 里那 5 条形参列表断言（原先是"已知缺口"断言，现在翻成正向）；`gaps.rs` 已支持清单 8 条 + 形状断言 3 条。

**处置**：半天。级别 B（报错，无静默风险）。

### B16. 相邻字符串字面量 —— 已修复

```cpp
const char *s = "a" "b";            // expected `;`
const char *msg = "line one\n"      // 同上 —— 长字符串按行折开的标准写法
                  "line two\n";
```

**性质**：缺规则（虽然是"翻译阶段 6"而不是文法规则）。**成因**：`parse_primary_expr` 的字面量分支读完一个 `StringLiteral` 就收工，第二个留给后面——初始化器在 `"a"` 处结束，于是声明报 `expected ;` 撞在 `"b"` 上。

**修复**：读完字符串后继续吃相邻的字符串字面量，全部留在**同一个 `LiteralExpr`** 节点里（它们本来就产出**一个**字符串）。带**用户自定义后缀**的字面量（`"a"_km`）结束这一串而不是加入——它是 `operator""` 的调用，不是字符串。

**发现经过**：真实 C 文件（LuaJIT 宿主）里一行 `const char *demo =` 后跟十个相邻字面量——这是长消息折行的唯一写法，所以形状遍地都是。

**护栏**：`expressions.rs::adjacent_string_literals_are_one_literal`（9 条 + "一个 `LiteralExpr`" + 后缀字面量结束串 + `1 2` 仍然报错）。

### B17. `for` 头的步进子句 —— 已修复

```cpp
for (;; i++) { }              // expected ), but get ++
for (;; i++, k++) { }         // 同上
for (i = 0; i < n; i++) { }   // 同上
```

**性质**：缺规则，而且是一处**只有 `for` 头才会踩到**的判据缺口。**成因**：步进子句和初始化子句共用 `parse_declaration_or_expression_statement_without_semicolon`（先试声明读法，失败回退表达式）。`i++` 的**声明读法**把 `i` 当成类型、产出**空声明符**、然后**成功**了——于是表达式读法根本没被尝试，游标停在 `++` 上，头规则报 `expected )`。

**为什么只有这里出问题**：普通语句里的 `i++;` 靠**自己的 `;`** 兜住——声明读法在 `++` 处失败，表达式读法接手。`for` 头没有那个 `;`，声明读法**没有什么必须撞上的东西**，于是空声明符"成功"了。这与 A0-1（赋值被读成声明）是同一个家族：**一个没命名任何东西的声明，凭什么算成功**。

**修复**（`decls.rs::parse_for_init_declaration`）：读完之后问一次"这条声明命名了东西吗"——用的是 A0-1 的同一个判据 `a_name_was_parsed`，界取在 specifier 序列之后（类型自己的名字在界之前，不能替声明符回答）。没命名就返回 `Err`，调用方回退成表达式读法。

**顺带修好的**：`for (v : items)`（C++20 省略声明的 range-for）现在也走表达式读法，语义更准；`for (auto x : items)` 等命名写法不受影响。

**护栏**：`expressions.rs::a_for_header_reads_its_step_as_an_expression`（10 条 + 形状断言：步进是 `ExpressionStat`，且整条函数体里只有函数定义是 `Declaration`）。

### B18. 用户自定义字面量作表达式 —— 已修复

```cpp
auto x = 1_km;        // expected primary expression
auto x = "a"_km;      // 同上
```

**性质**：缺规则（一个分支）。**成因**：词法器**特意**为它产出一个 `UserDefinedLiteral` 类型（源码注释就写着"解析器不能把它当普通数字"），而 `parse_primary_expr` 的字面量分支**没有收这个类型**——一张空头支票的镜像版本：不是"表里有节点没规则产出"，而是**词法层特意分出来的东西，语法层没人接**。

**修复**：把 `UserDefinedLiteral` 加进字面量分支（以及"这个实参列表里是值吗"的扫描，保持一致）。

**发现经过**：写 B16 的测试时顺带撞上的——**写反面用例是发现邻近缺口的有效手段**，这一条值得记进维护约定。

**护栏**：`expressions.rs` 的 B16 测试里含后缀字面量两条；`gaps.rs` 已支持清单 2 条。

**B16 的补充（宏在字符串中间）**：`"compiler[" COMPILER_ID "]"` —— 字符串 run 里出现**标识符**也算同一串。这不是文法规则，是关于**预处理**的陈述：展开之后它就是一个字符串，而"字符串后面跟一个名字"在别的读法下根本不是合法 C++。parser 不跑预处理器，所以这是它必须接受的形状——CMake 生成的编译器识别文件全是这个。`parse_primary_expr` 的 run 循环因此也接受 `Identifier`。

### B22. clause 里**带括号**的比较 —— 已修复

```cpp
template <int N> requires (N < 0 || N > 3) void f();   // 曾报 expected ), but get integer literal
template <typename T> requires (C<T>) T value = T{};    // 修 B22 时牵出的同族问题
```

**性质**：缺规则（B21 留下的边界，靠一个**新判据**解决而不是靠调停表）。**成因**：B21 的第二层（"后面跟着操作数就不是 template-id"）在 clause 内被关掉了，否则约束后面的**声明**会被当成操作数——而这里的操作数**在括号里面**，属于约束本身。两种情况需要区分。

**修复**：区分它们的是**括号是否还开着**，而且这是可判定的，不是猜的：

* `ParenExpr` 还开着 → **约束本身还没结束**，那个操作数不可能是声明 → 第二层照常生效；
* clause 顶层（没有括号开着）→ 操作数只能是跟着 clause 的声明 → 第二层不生效。

新增 `CppParser::is_open(kind)`：`open_marks` 是 parser 自己记的"还开着的节点的**事件位置**"，位置是私有的但事件流是公开的（`events()`），两者一拼就能回答"某个 kind 现在开着吗"。**没有加字段**——`open_marks` 里找 kind 就够了，代价是 O(开着的节点数)。

**顺带修掉同族的一个**：cast 的operand 判据（T1）在 clause 里也会误伤——`requires (C<T>) T value = T{};` 里 `(C<T>)` 后面跟着 `T`，被读成 `(C<T>)T` 这个 **cast**，于是声明没了名字、报 `expected a declarator name`。两条规则问的是同一个问题，所以抽成 **`an_operand_is_decisive`** 一处：

```rust
!p.is_in_a_constraint() || p.is_open(CppSyntaxKind::ParenExpr)
```

**这一条是"两处规则共用一条判据"的第三次**（前两次：`starts_an_operand` 共用于 cast 与 template-id；`Level` 共用于所有列表读取器）。写第二次的时候把它抽出来，就不会有第三次的漏改。

**护栏**：`concepts.rs::a_constraint_keeps_its_template_id_when_a_declaration_follows`（原先是"已知缺口"断言，现在翻成正向：5 条约束后的声明 + 5 条括号内比较 + 形状断言 `BinaryExpr == 3`、`TemplateArgumentList == 0`）；`gaps.rs` 已支持清单 3 条。

### B23. 构造**中间**的预处理条件行 —— 已修复（两种形状）

```c
char const info_version[] = {
  'I', 'N', 'F', 'O', ':',
#ifdef COMPILER_VERSION
  COMPILER_VERSION,
#endif
  '\0' };
```

**现象**：`#ifdef` / `#elif` / `# endif` 出现在**一个初始值列表或表达式中间**时报 `expected primary expression`。语句层能处理指令（`parse_declaration_or_expression_statement` 见到 `#` 就交给 `parse_preprocessor_directive`），但那条路只在**语句边界**上；指令落在构造内部时没人接。

**性质**：缺规则，但根源是"不做预处理"这个立场本身留下的边界。**唯一证据来源**：CMake 生成的 `CMakeCCompilerId.c`（真实项目里到处都有这个文件），B21 修好之后它是 `luajit-dll` 里唯一还有报错的文件（10 条，全部集中在条件行及其级联）。手写的 C 文件（`main.c`、`extensions.c`）和 lua 的头文件**全部 0 报错**。

**修复**：在**两个具体位置**接管，都不是"到处都能跳指令"那种大改：

1. **花括号初始化列表的元素之间**（`parse_braced_initializer`）：循环顶上见到 `#` 就读成指令节点（树仍然无损、消费者仍然看得到），而且**元素后面的逗号检查也接受 `#`**——整行元素都可能是条件编译出来的，那种写法没有逗号。
2. **字符串字面量串中间**（`parse_primary_expr` 的字面量分支）：
   ```c
   const char* info = "INFO" ":" "extensions["
   #if defined(__clang__)
     "ON"
   #else
     "OFF"
   #endif
     "]";
   ```
   串里见到 `#` 就把指令读成节点再继续串。**一个 `#` 在这里不可能是别的意思**——"字符串字面量后面跟指令"在任何读法下都不是 C++；而**别处的 `#` 仍然报错**（`x = 1 # 2;` 依旧报），这一条是防止定点接管掩盖真错误的关键。

**为什么不做"任何位置都能跳指令"**：那等于把指令当 trivia，会让"指令必须是节点、树要无损"这条既有决定作废，而且会吞掉真正的错误（`x = #if` 也会被跳过）。按维护约定第 5 条，这种改动要先列调用点；而真实代码里被撞到的**只有上面两种形状**，定点修完就够——`CMakeCCompilerId.c` 的指令类报错从 10 条降到 **0** 条。

**仍不支持的**（真实文件里没碰到，登记备查）：指令出现在**任意两个表达式 token 之间**，例如 `int x = 1 +` 换行 `#ifdef A` … `2;` … `#endif`。支持它要让表达式规则在运算符边界跳指令，风险同上。

**护栏**：`expressions.rs::a_directive_inside_a_literal_run_is_read_as_a_directive`（2 条 + "一个 `LiteralExpr`" + "两个指令节点都在树里" + "别处的 `#` 仍报错"）；语料与真实文件探针。

**顺带**：`CMakeCCompilerId.c` 剩下的 3 条是 **K&R 风格函数定义**（`int main(argc, argv) int argc; char *argv[];`），那是**另一个缺口**，与本条无关——后来单独修了，见 B24。

### B24. K&R 风格函数定义（形参声明写在括号外） —— 已修复

```c
int main(argc, argv)
    int argc;
    char *argv[];
{ return argc; }
```

**现象**：`int main(argc, argv)` 后面跟着一个声明时报 `expected primary expression`（诊断落在那个声明上，整条定义随后级联报错）。C++ 已废除、C 已弃用，但 1989 年前的 C 全是这么写的，而**发现它的文件是 CMake 生成的 `CMakeCCompilerId.c`**——每个 CMake 构建树里都有一份，不是罕见文件。B23 修完后它是 `luajit-dll` 里唯一还有报错的文件。

**成因**：缺规则，而且是"括号自己说不了"的那种缺。`(argc, argv)` 读在任何东西说明它是哪种表之前，两种读法的 token 完全一样：`argc` 是**完全合法的形参类型**，所以现代形参表把这两个名字读成两个没有名字、类型分别是 `argc` 和 `argv` 的形参。分辨两者的是**后面**——现代函数接着 `{`、`;`、`:`、`requires`……而"函数声明符后面跟一个声明"只可能是 K&R 形参表。这正是维护约定第 13 条要问的两句（读完停在哪、后面能跟什么），只是这次的答案是"能跟一个声明"。

**修复**：`finish_init_declarator` 收尾的 match 加一条兜底分支（`declarator_is_function && starts_an_old_style_parameter_list(p)`），把括号后面那些声明读进新 kind `OldStyleParameterList`（紧跟 `ParameterList` 定义），其下逐条调用**普通声明规则**；判据只问"这里能不能开始一条声明"（类型关键字 / `starts_declaration` / 本文件已知的类型名）。**两种答案都留在树里**：`ParameterList` 是括号说的，`OldStyleParameterList` 是它实际的意思——消费者要形参的类型就读后者，`old_style.rs` 把这一对钉住了。

**第二个缺陷（修好第一个才露出来）**：K&R 头的**最后一个形参声明已经吃掉了 `;`**，外层声明因此没有自己的 `;` 可要；只有带函数体的写法（游标正好落在 `{` 上）才碰不到这条。而 CMake 文件恰好是**没有函数体**的形状——头和体分处同一个条件的两支，体由两支共用：

```c
#if defined(__CLASSIC_C__)
int main(argc, argv)
int argc;
char* argv[];
#else
int main(int argc, char* argv[])
#endif
{ … }
```

于是它报 `expected ';'`，而诊断打在 **`#else` 那一行**上：一条报错落在预处理行，整条定义连同**没写错的那一支**一起没了。判据：本声明的事件里出现过 `OldStyleParameterList` 就不再要收尾 `;`——token 已经把树定死了，后面跟着什么由各自的规则负责。这是"一个缺陷遮住另一个缺陷"的又一次：不先修好第一条，根本走不到第二条。

**性质**：缺规则（C 的旧写法；本 parser 的既定目标包含读 C）。修完 `luajit-dll` 的**全部**真实文件——`main.c`、`extensions.c`、`extensions.h`、`lauxlib.h`、`lua.h`、`luaconf.h`、`lualib.h`、`CMakeCCompilerId.c`——都是 **0 报错**，这是本项目第一次做到一个真实项目的文件全绿。

**护栏**：`tests/old_style.rs`（7 条：一条定义的整体形状、两种列表并存、前面有别的声明、**无函数体**的头、头体分处条件两支、现代声明符不长出这个节点、列表到非声明为止）+ `gaps.rs` 的已支持清单与形状断言 + 真实文件探针。

### B25. 函数 try 块（`void f() try { } catch (...) { }`） —— 待办

**现象**：`try` 出现在**声明符和函数体之间**时报 `expected primary expression`（后面级联若干条）。语句形式的 `try { } catch (const E& e) { }` 是支持的（在 `gaps.rs` 的已支持清单里）。

**成因**：缺规则。声明符收尾处只认 `{`、`;`、`:`、`requires` 和 K&R 形参表，没有 `try`；而函数 try 块的 `try` 必须在那里被接住，之后是可选的构造函数初始化列表、`catch` 序列，最后才是 `{`。

**性质**：缺规则，**响亮**（报错，不静默）。真实代码里罕见——构造函数在成员初始化失败时才用——成本按小算，所以登记而不是立刻修。

**发现经过**：写 B24 的护栏时，为了说明"现代函数声明符后面能跟什么"随手列了一条 `void f() try { } catch (...) { }` 当反例，结果它自己读不出来。维护约定第 12 条（写反面用例也会撞出缺口）的又一次。

**护栏**：`gaps.rs` 的"仍不支持"清单（`Where::Class`：`void f() try { } catch (const E& e) { }`）。

## 一批：真实 C++ 项目探针暴露的 10 类（B26–B35）

**共同的发现经过**：`luajit-dll` 全绿之后，登记表在 **C** 上收敛了，但 C++ 侧从来没有拿真实项目当过探针——`real_world.cpp` 只有 2.9KB 手写片段，其余全是单元测试。把 `cpp_dump` 指向 `EmmyLuaCodeStyle`（200 个 C++ 文件，真实工程）后，**66 个文件报错**，而按"每个文件的**第一条**错误"归因只有 **10 个根因**（其余全是级联，最多的一份文件报了 98 条）。十条都做了最小复现并逐条孤立确认，下面各记一条，末尾附**修完之后的复验结果**。**一次探针挖出的比前面手写清单加起来还多**——维护约定第 11 条的第三次验证。

**复验（十条全部修完之后，同一个 200 文件工程、同一把 `cpp_dump`）**：

| | 修前 | 修后 |
|---|---|---|
| 有报错的文件 | 66 / 200 | **7 / 200** |
| 报错总数 | ~650 | **128** |

剩下的 7 个文件归成 **5 个新成因**，已逐条最小复现（见本节末"下一批"）。也就是说：**这一轮把"真实文件里能看见的问题"从 10 类压到 5 类**，而那 5 类里没有一类是这一轮改出来的。

### B26. 枚举量的初始化式**吃掉后面的枚举量** —— 已修复

```cpp
enum E { A = 0, B };        // 干净、无损、良构、零报错 —— 但 B 不再是枚举量
enum E { A = 0, B, };       // 同一个成因，带尾逗号时响亮报错
```

**现象**：第一种写法树里只有**一个** `EnumeratorDecl`，`B` 变成 A 的初始化式里的 `BinaryExpr`（`0, B`）：消费者问枚举有哪些成员，从第一个带初始化式的成员之后全丢。第二种写法报 `expected primary expression`（诊断落在 `}` 上）。EmmyLuaCodeStyle 里 2 个文件就是它。

**成因**：`parse_enumerator_body` 用 **`parse_expr`** 读初始化式——那是**逗号运算符**那一层，于是 `= 0` 顺手把 `, B` 也吃了。这与当初**位域宽度**的缺陷是同一个：一条**自己拼逗号**的规则（枚举体拼 `,`、位域表拼 `,`）必须退出逗号运算符。位域那处当时改成了 `parse_assignment_expr`，这里漏了——**同一个教训的第二处，隔了几轮才撞上**（第一处见维护约定第 2 条那句"一条规则自己拼逗号就要退出逗号运算符"）。

**为什么既有护栏没抓到**：`gaps.rs` 与文档里的枚举例子一直是 `enum Color { Red, Green };`——**没有初始化式**。所以这类"构造在、形状对、只有一半读法"的缺陷，靠"这个构造能读吗"是问不出来的。这是 A0 类（静默错树），也是本批唯一静默的一类。

**性质**：A0。

**修复**：`parse_enumerator_body` 的初始化式改用 `parse_assignment_expr`（位域宽度当年就是这么修的），一行；**护栏**是"枚举成员数"的形状断言（`gaps.rs`：`enum E { A = 0, B }` 必须是 **2** 个 `EnumeratorDecl`，另加带尾逗号、`enum class : unsigned`、多个带初始化式四种），以及已支持清单里的四条枚举写法。**这一条也说明了护栏该问什么**：`enum Color { Red, Green };` 钉了十年，钉的是"能读"，而该问的是"读出了几个成员"。

### B27. UTF-8 BOM 报 `unrecognized character` —— 已修复

```
EF BB BF 23 70 72 61 67 6D 61 20 6F 6E 63 65     ← BOM + `#pragma once`
```

**现象**：文件开头带 UTF-8 BOM 时，**第一个 token 就报错**：`unrecognized character \u{feff}`。EmmyLuaCodeStyle 里 **37/200 个文件**如此（Visual Studio 与一批 Windows 编辑器存盘默认加 BOM），是本批覆盖面最大的一类。诊断在偏移 0，等于"文件一开头就坏"，对编辑器是最刺眼的一种坏法。

**成因**：词法器的字符分派里没有 U+FEFF，落到"未知字符"兜底分支。U+FEFF 是零宽不换行空格，C++ 标准把 BOM 当**空白**处理，本来就该进 trivia。

**性质**：缺规则（词法层），成本最低、收益最大。

**修复**：`lex()` 的字符分派把它并进空白分支，`lex_whitespace` 的谓词也收它（文件中间的同一个码点同样当空白）。**护栏**：`lexer.rs::a_byte_order_mark_is_whitespace`（无报错 + 是一个 `Whitespace` token 而不是被丢掉——这同时是"不丢字符"的检查 + 中段 FEFF）+ `gaps.rs` 已支持清单里的 `\u{feff}int x;`。

### B28. 块作用域的直接初始化判据**只看到一层** —— 已修复

```cpp
LuaParser p(file, std::move(luaLexer.GetTokens()));   // expected `;` after expression
Widget w(a, std::move(b));                            // 干净
Widget w(a, f(b.c()));                                // 干净
Widget w(a, std::move(b.c()));                        // 失败
```

**现象**：`类型 名字(实参…)` 在**函数体内**读不出来（文件作用域同理），报 `expected ; after expression`；EmmyLuaCodeStyle 里 7 个文件的第一条错误都是它（`LuaParser p(file, std::move(luaLexer.GetTokens()))`、`std::fstream fin(newPath, std::ios::in | std::ios::binary)`）。

**成因**：`direct_init` 的判据问的是"**实参看起来像值吗**"，而"像值"的证据是在实参里找字面量/调用/运算符。`std::move(b)` 能看出是调用，`std::move(b.c())` 却看不出——**证据是嵌在里面的**（限定名调用里再套成员调用），扫描没往里走两层。`std::ios::in | std::ios::binary` 同理：两个限定名之间的 `|` 没被当成"只可能是值"的运算符。

**性质**：缺规则（判据深度不足）。**注意它和 B21/B22 是同一族**：判据看着"够用"就收工，遇到更深一层的嵌套就退回错的读法。

**修复**：`the_arguments_like_values` 里"元素由名字后面那个 token 判定"这句话，改成"由**整条限定名链**后面那个 token 判定"——`::` 只说明名字还没完，不是答案。`std::move(b)` 于是由 `move` 后面的 `(` 判成调用。

**顺带记下一条容易看错的现象**：`Widget w(a, std::move(b));` 本来就"干净"，但**它是函数声明**（形参类型 `std::move`），不是直接初始化——形参表读法先试且成功，根本轮不到这条判据。所以只有"形参表读不出来"的形状（`std::move(b.c())`、`B::C(1)`、`ios::in | ios::binary`）才会暴露这个缺陷。找根因时这一点很关键：**"干净"不等于"读对了"**（第 13 条的老问题，这次出现在探针的判读上）。

**护栏**：`direct_init.rs::a_qualified_name_is_judged_after_the_whole_chain`（5 条限定名调用必须读成直接初始化 + 4 条形参表读法不受影响）。

### B29. 声明开头的"宏 + 类型名"两个裸名字 —— 已修复

```cpp
EMMY_API RangeFormatResult f(const char *code);   // 读不出来
EMMY_API char *f(const char *code);               // 干净（char 是关键字）
MY_API W *f();                                    // 也读不出来（W 已知为类型也没用）
```

**现象**：导出宏（`MY_API`/`EXPORT`/`EMMY_API`/`__declspec` 之类）写在返回类型前面时，**只要返回类型不是关键字**，整条声明读不出来（退化成表达式语句，报 `expected ; after expression`）。真实 C++ 工程里这种前缀宏遍地都是。

**成因**：声明说明符序列接受"一个裸名字 + 一个关键字类型"，不接受"**两个连续的裸名字**"——`EMMY_API RangeFormatResult` 是两个名字，判据（`declarator_starts_with_a_type_keyword` 那一族锚点）不成立，于是声明读法没被尝试。**已知为类型也救不了**：那条路只在名字**后面**紧跟声明符时才用得上。

**性质**：缺规则。修法要在"说明符里已经有名字了"时仍允许再吃一个名字，且不能把 `A * b;`、`g(1, 2);` 这些既有判据弄坏（维护约定第 5 条：改公共入口先列调用点）。

**修复**：`name_joins_the_type` 多问一句——"这个名字后面还留着**声明符**吗？"（后面是名字 / `*` / `&` / `&&` / `::` / `<` 就是留着，是 `;` `)` `,` `=` `(` 就不是），留着就让它并入类型。两条例外都是必需的，缺一条就出事：

1. **只在声明里问，不在 type-id 里问**（`allow_second_name`）。第一版漏了这条，于是 `-> int requires C<T>` 里的 `requires` 也"后面留着声明符"，整个 clause 被吃进返回类型——`gaps.rs` 的形状断言当场抓出来（`no RequiresClause in it`）。**这就是维护约定第 6 条的价值**：报错、无损、良构三样全过，只有"这个构造必须读成这种节点"能拦住。
2. **只在已经有一个名字并入类型之后才问**（从事件流读回 `TemplateType`，不是加一个标志位——理由和 `has_type_specifier` 一样：忘了置位的分支是静默的）。

**代价**：`Name Name Name`、`Name Name * Name` 在任何语法里都不是表达式，所以只有"原本没有读法"的语句改变了读法。

**护栏**：`ast.rs::an_export_macro_before_the_type_leaves_the_declarator_last`（**用类型化 AST 问"这条声明叫什么名字"**——要的是 `p`/`f`/`g`/`make`，而不是"能解析"；加 4 条不受影响的对照）+ `gaps.rs` 已支持清单三条。

### B30. 嵌套 `>>` 与空模板实参 `std::less<>` —— 已修复

```cpp
class S : public DBBase<K, std::shared_ptr<V>> { };              // expected `;`
void f(const std::map<int, int, std::less<>> &m);                // 读不出来
std::map<int, int, std::less<>> &Get();                          // 干净
void f(std::less<> l);  void f(std::shared_ptr<A::B> p);         // 干净
```

**现象**：两处各自成立、**组合起来就坏**：基类子句里嵌套 `>>` 收尾报 `expected ;`（2 个文件），形参里"空模板实参 `<>` 套在另一个 template-id 里"（连排三个 `>`）读不出来（3 个文件，含 `std::less<>` 作返回类型时级联到下一行）。

**成因**：`>` 的配对与"这个 `>` 是模板收尾还是比较"这条判据（B21）在**嵌套 + 连排**时数错了一格：`less<>` 的 `<>`、`map<...>` 的收尾 `>`，三个 `>` 挤在一起，配对方向就丢了。

**性质**：缺规则（判据在连排 `>` 上的漏洞）。

**修复（三处，都是同一个"一个 token 关两层"的问题）**：

1. **模板实参表**：`parse_template_argument_list` 的循环只在**读完一个实参之后**才 `split_closing_angle`，而**空实参表**没有实参可读——游标一上来就是 `RightShift`，于是报 `expected a template argument`。修法是在循环顶上先拆。非空写法一直好，只是因为它的实参先被读了。
2. **`a_body_follows_the_class_head`**（类头的 `{` 预查）：它数 `<` / `>` 时漏了 `RightShift`，于是嵌套 template-id 的基类子句里深度一直没归零，`{` 到不了、类头被当成"没有类体"，整条定义报 `expected ;`——而**单层**实参表的同一个基类子句是好的。
3. **抽出 `angle_depth_delta`**：数角度的扫描一共有**三处**（限定名预查、`a_parenthesis_follows_the_name`、类体预查），前两处有 `RightShift`、第三处没有。这正是维护约定第 14 条说的事——**同一个判据的第二处用法就是例外被漏掉的地方**，所以这次不补第三遍，抽成一个函数。

**顺带**：修 B29 时把 `<` 也算作"后面留着声明符"，当场撞坏了**显式实例化** `template MyType f<int>(int);`（`f<int>` 是声明符的名字，不是类型的一部分）——`modern.rs` 的既有测试抓住了。判据要走过**整个实参表**再看后面那个 token：`*` → 名字属于类型（`MY_API Vector<int> *make();`），`(` → 名字是被调用的那一个（`f<int>(int)`）。

**护栏**：`gaps.rs` 已支持清单五条 + 形状断言（嵌套基类必须有 `BaseSpecifier`；`std::less<>` 那条必须有**两个** `TemplateArgumentList`）。

### B31. 成员指针：`->*` 与 `(Class::*name)` —— 已修复

```cpp
(c->*h)(1);                                                   // expected ), but get ->*
std::shared_ptr<ReturnType> (LSPHandle::*handle)(std::shared_ptr<ParamType>)   // 类体提前收尾
```

**现象**：`->*`（指向成员的指针解引用）没有规则，`(this->*handle)(...)` 直接报错（`CodeActionService.cpp`）；类里的**成员函数指针形参** `(LSPHandle::*handle)(...)` 更坏：它让**类体提前收尾**，其后的成员全部级联报错（`LSPHandle.h` 的 2 条报错是它，第 2 条落在类的 `};` 上）。

**成因**：`->*` 已在词法器（`ArrowStar`），表达式层没有对应的读取；声明符层的 `::*`（指向成员的指针声明符）没有规则，于是 `(LSPHandle::*handle)` 这个括号读法失败，`}` 被当成类体结束。

**性质**：缺规则（两处，同一个概念）。

**修复（三处，其中一处是探针自己挖出来的静默错树）**：

1. **表达式层**：`.*` / `->*` 加进运算符表，优先级 14（标准里 pm-expression 在 multiplicative-expression 之下，比乘法结合更紧）。
2. **声明符层（静默那半）**：`int C::*p;` 本来"能读"，但**读错了**——`C::` 并进了类型，`*p` 成了**类型节点里面的** `InitDeclarator`：无损、良构、零报错，而消费者问类型得到 `int C::`、问声明符什么也得不到。判据：`continues_a_qualified_name` 遇到"`::` 后面紧跟 `*`"要回答**否**——那是成员指针运算符，不是限定名的延续（`int A::B *p;` 是"类型 `A::B` + 指针 `p`"，两者的区别后一个 token 说了算）。
3. **抽象声明符**：`Class::*` 属于 ptr-operator（标准里 nested-name-specifier 在运算符**里面**），所以那个类名站在抽象声明符的起点而不是声明符名字的起点。加上这条之后 `int C::*p;` 的树才正确；再把"括号里是成员指针运算符"补进另外两条括号判据，`typedef int (C::*fp)(int);`、`void g(int (C::*h)(int));`、无名参数的 `void h(int (C::*)(int));` 一起通了。**有名字的 `(C::*h)` 与无名字的 `(C::*)` 必须分开判**——前者归 `parse_parenthesised_declarator`（它要求有名字），后者归抽象声明符；混在一起会让无名的那个参数读不出来。

**护栏**：`operators.rs::the_pointer_to_member_operators_are_binary_operators`（5 条 + 必须有 `BinaryExpr`）+ `gaps.rs` 已支持清单六条 + **形状断言**（`int C::*p;` 的 `DeclSpecifierSeq` 文本必须恰好是 `"int "`——这是唯一能看见"声明符钻进类型里"的判据）。

### B32. gtest 风格的"宏 + 块" —— 已修复

```cpp
TEST(FormatPerformance, 1k_row) {
    ...
}
```

**现象**：文件作用域上"一个调用后面直接跟块"读不出来，报 `expected ; after expression`（`{` 无处安放）。EmmyLuaCodeStyle 的 6 个 `*_unitest.cpp` 如此，其中两份报了 68 和 98 条——**全是这一条的级联**。

**成因**：`TEST(A, B)` 没有声明符名字，声明读法失败；它不是函数定义，也不是变量声明——真实含义是**宏**，而"宏 + 块"这种形状（gtest/Catch2/benchmark 都这么写）没有规则。字面地看，它和"函数定义的头"只差一个名字。

**性质**：缺规则。修法要克制：只在**文件/名字空间作用域**、且"调用形状 + 块"时才接，别把 `g(1,2) { }` 这类真错误也吞掉。

**修复**：新增形状判据 `a_macro_definition_follows`（无声明符名字 + 头的名字不是限定名 + 不在函数体内 + 括号是**平衡**的且后面紧跟 `{`），命中就把括号当**宏的实参表**读成 `ArgumentList`（原始 token，不做任何解释），并把"这是函数声明符"的标志置上，好让 `{` 成为**函数体**。三条例外都是被既有测试逼出来的：

1. **限定名头部**（`void Widget::draw(T) { }`）：它的括号**就是**形参表——第一版把它的形参表吃成了实参表，`direct_init.rs` 的两条既有测试当场报 `the parentheses are a parameter list`（0 ≠ 1）。
2. **不在函数体内**：`g(x) { }` 在函数体里是"语句后面跟一个块"，那是真错误。
3. **判据必须同时加进声明符后缀循环的开门条件**（`types.rs`）：循环原本只在"声明/表达式问题有答案"时打开，而这个形状**恰恰没有答案**（宏的 token 既不像值也不像声明符）——所以 `TEST(A, B) { }`（实参像声明符）能读、`TEST(A, 1) { }`（实参像值）不能读。同一个判据写两遍就是维护约定第 14 条的场景，所以抽成一个函数两处调用。

**护栏**：已支持清单（**文件作用域**四条：`TEST(A, B) { }`、带 `1k_row` 的、体内有语句的、名字空间里的）+ 形状断言（块是 `Declaration` 的**子节点**、其文本是 `{ int x = 1; }`——即测试体真的被读成了体）。

### B33. 用户自定义字面量作**实参** —— 已修复

```cpp
auto x = 1_km;      // 干净（B18 修的）
g(1k_row);          // expected ), but get identifier
```

**现象**：B18 让 `UserDefinedLiteral` 能被表达式读法接住，但**实参列表**里的同一个 token 仍然失败。`Performance_unitest.cpp` 的 `TEST(FormatPerformance, 1k_row)` 里就有一个。

**成因**：典型的"空头支票的镜像"（维护约定第 12 条）：上游（词法器）特意分出了 `UserDefinedLiteral`，下游**一处**接了（字面量分支），另一处（"这里能开始一个操作数吗"的判据）没接。

**性质**：缺规则（判据表漏一项）。

**修复**：真正的成因在**词法层**，不在判据表：后缀只有以 `_` 开头才被当成用户自定义后缀，于是 `1k_row` 被切成 `1` + `k_row` 两个 token，`g(1k_row)` 的实参读法自然失败。改成**任何标识符**都可以是后缀之后，`1k_row`、`100ms`、`"name"sv` 都是一个 `UserDefinedLiteral`，而标准后缀（`1u`、`1LL`、`1z`）、指数（`1e5`）、十六进制（`0x1f`）、浮点后缀（`1.5f`）都在到达这条规则之前就被数字扫描吃掉了、行为不变。不带 `_` 的后缀在标准里只是"保留给实现"（对**程序**的要求，不是对 token 的要求），GCC/Clang 都按一个字面量收下并给个警告。

**顺带**：这条比 preprocessor 更早生效，因为 `1k_row` 在预处理阶段本来就是**一个 pp-number**——这也解释了为什么 gtest 的 `TEST(FormatPerformance, 1k_row)` 里那个"测试名"能这么写。

**护栏**：`lexer.rs::a_literal_suffix_need_not_start_with_an_underscore`（8 条必须是一个 `UserDefinedLiteral` + 9 条标准后缀不得变成它 + `12abc` 是一个 token）+ 已支持清单三条。

### B34. 表达式里以 `::` 开头的限定名 —— 已修复

```cpp
return (::abs(static_cast<int>(x)) > y);   // expected ), but get >
return (std::abs(x) > y);                  // 干净
```

**现象**：`(::abs(x) > 1)` 报 `expected ), but get >`（`SymSpell.cpp`）；把 `::` 换成 `std::` 就干净。

**成因**：括号读法（`ParenExpr` / cast 的两义）里，"全局限定名 `::name`"这种操作数不在"这里能开始一个操作数"的判据表里，于是 `(` 后面那一串被当成类型读法的一部分，读到 `>` 就要 `)`。

**性质**：缺规则（判据表漏一项，和 B33 同族）。

**修复**：问题不在"操作数从哪儿开始"的判据表，而在 **`is_a_type_in_parentheses`**——它有一条 `Some(&Scope) => true`，意思是"以 `::` 开头的一定是类型"。那正是 T1 那条教训的反例：`::name` 既是全局限定**类型**的开头，也是全局限定**表达式**的开头，而 token 上分不出来，唯一能分的是 `)` **后面跟什么**。那条判据（"`(` 后面是类型"）本来就在上面几行、且已经在用操作数证据，所以**删掉这个 arm** 就够了：`(::x)`、`(::abs(x) > 1)` 回到括号表达式，而 `(::MyType)x`、`(::std::string)x`、`(::MyType*)p` 因为有操作数/指针运算符照旧是转换。

**顺带**：这也是维护约定第 14 条的反向用法——**判据的每一条例外都要能被"后面跟什么"解释**。裸名字那一条已经用类型表 + 操作数证据回答了，`::` 那一条却只凭"看起来像类型"。

**护栏**：`operators.rs::a_global_qualified_name_in_parentheses_is_an_expression`（6 条必须是 `ParenExpr` + 3 条必须仍是 `CastExpr`）+ 已支持清单两条。

### B35. 函数式转换 `bool(x)` —— 已修复

```cpp
root.AddChild("code_style_check", bool(lint["codeStyle"]));
```

**现象**：`bool(x)`、`int(y)` 这类**函数式转换**（用 builtin 类型名当函数调用）报 `expected primary expression`（`ClientConfig.cpp`）。

**成因**：表达式里"类型名 + `(`" 只有两条路：显式类型转换 `(T)x` 和构造临时对象 `T(x)`；后者要求 `T` 是**已知类型名**，而 `bool`/`int` 是关键字，走的是另一张表，那张表没接 `(`。

**性质**：缺规则（关键字类型名没接函数式转换）。

**修复**：`parse_primary_expr` 加一条分支——类型关键字 + `(` → `CastExpr(BuiltinType(关键字), ParenExpr(操作数))`。**类型只吃关键字本身**：交给 `parse_type_id` 会把括号读成函数类型的形参表（"返回 bool、形参 x 的函数"），然后没有 `(` 留给载荷了（报 `expected (, but get ;`）——这是第一版的错。声明读法照旧优先：`int(x);` 是"声明了带括号的名字 `x`"，测试里钉住了这条对照。

**护栏**：`operators.rs::a_functional_cast_with_a_keyword_type_is_a_cast`（6 条必须是 `CastExpr` + `int(x);` 必须不是）+ 已支持清单两条。

### 第二批：复验后剩下的 5 个成因（B36–B40）—— 已全部修复

成因逐条最小复现过，修法与"被什么逼出来的"记在下面。**跑完这一批的复验**：同一把 `cpp_dump`、同一个 200 文件工程，有报错的文件 **7 → 1**，报错总数 **128 → 64**，而**剩下的 64 条全部来自同一个新成因**（见 B41）。

**共同的方法论收获**：这一批里有**三条**的"最小复现"一开始都指向了错的地方——

* **B36** 表面是"宏 + 块"，实际要先问"**在哪一层**"：文件作用域上这个形状没有别的读法，函数体里它和一个真错误（调用漏了 `;` 后面跟一个块）同形；
* **B37** 表面是"无名类类型"，实际是"**类体后面的名字**"——`struct S { … } x;` 一样坏，而且**静默**（`x` 併进了类型，声明什么都没声明）；只在声明符带东西（`= { … }`、`[N]`）时才响亮；
* **B38** 表面是"函数类型进形参"，实际是"**函数类型当模板实参**"——`sizeof(bool(T))` 是好的，`sizeof(A<bool(T)>)` 坏，而根因在**数尖括号的那次预查**：它把第一个 `)` 当边界，于是模板实参表根本没被尝试，文件作用域上 `A<bool(T)> x;` 被读成**比较表达式**（A0，静默）。

| 编号 | 例子（最小复现） | 现象与真因 | 归属文件 | 状态 |
|---|---|---|---|---|
| **B36** | `void f() { IF_EXIST(k) { g(); } }` | 报 `expected }, but get ;`。**函数体内**的"宏 + 块"：文件作用域那条规则（B32）故意不收体内，因为体内同形的是一个调用漏了 `;`。修法是**按宏的书写约定**区分——全大写下划线才算宏，于是 `IF_EXIST(k) { … }` 读得出来而 `g(x) { }` 仍然是错误 | `LuaStyle.cpp`（68 条级联） | **已修复** |
| **B37** | `static const struct { unsigned char left; } priority[] = { { 1 } };` | 报 `expected a declarator name`。真因是**类体后面的那个名字**：`type_is_already_complete` 向后走，遇到 `}` 一律回答"还没有类型"（对"大括号是外层块的结尾"是对的），于是名字并进了类型。`struct S { … } x;` 静默错树，带上 `= { … }` 或 `[N]` 才响亮 | `LuaDefine.h`（45 条） | **已修复** |
| **B38** | `A<bool(T)> x;`、`sizeof(A<bool(T)>)` | **函数类型当模板实参**时，模板实参表根本没被尝试：`a_matching_angle_bracket_follows` 把第一个 `)` 当边界，而函数类型的括号是**配对**的、属于实参表。文件作用域上它被读成比较表达式（A0） | 3 个文件 | **已修复** |
| **B39** | `struct S { void f() { for (auto &v: vec) { } } };` | 报 `expected ;, but get )`。位域判据问的是"上面有没有类体"而不是"**最里层的大括号是不是类体**"，于是成员函数体里的范围 `for` 的 `:` 被当成位域宽度（`v: vec`）。改成用一个**大括号栈**（类体 / 块）来判断 | `LSP.h`（5 条） | **已修复** |
| **B40** | `try` … `#if` … `{` 与 `}` … `#if` … `catch` | 报 `expected }`。指令落在 **`try` 与它的块之间**（第二种：`}` 与 `catch` 之间）——B23 那一族的形状，只是接缝在 `try` 上，而且**两处接缝都要接**：只补 `}`/`catch` 那处，真实文件仍然报错 | `IOSession.cpp`（5 条） | **已修复** |

### B44. 链接块里的指令 —— 已修复（`winnt.h` 那一族的根）

```cpp
#ifdef __cplusplus
extern "C" {
#endif
int x;
#ifdef __cplusplus
}
#endif
```

**现象**：`#endif` 被读成一个 token 宽的 `ErrorNode`，紧接着 `endif` 成了下一条声明的**类型名**（`endif int x;`），
链接块的 `}` 从此再也对不上，循环一直跑到文件结尾——`winnt.h` 因此变成**一个 `CompoundStat` 覆盖 387 000 字节**，
里面有 **八条 `#endif` 是扫描器看不见的**。

**成因**：`parse_linkage_block` 的循环只调 `parse_declaration`，**没有指令分支**；而这是**每个 C 头文件**写
链接块的方式（`winnt.h:11`）。

**性质**：缺规则（接缝）。这是"指令落在构造的接缝上"（§2.1）那一族在**链接块**上的实例——九处接缝都补过，
漏了它，因为它的症状不在本文件里：**丢的是指令，坏的是整份文件的条件嵌套**，而条件嵌套是宏层（哪个宏在这里
生效、分支结论）唯一的输入。

**护栏**：`gaps.rs::a_linkage_block_keeps_the_directives_written_inside_it`（块内两条指令都是指令节点、
块内没有 `ErrorNode`、块止于自己的 `}`）。**量到的**：`winnt.h` 的指令从 4025 个节点 / 936 条丢失 →
**4090 个节点 / 0 条丢失**；455 文件闭包 干净 328 → 329。

### B45. 多声明符的 `typedef` —— 已修复（一族，不是一个文件）

```cpp
typedef WCHAR *PWCHAR, *LPWCH, *PWCH;          winnt.h 里成百上千行
typedef int A, B;
```

**现象**：第一个 `,` 处报 `expected ;`，后面的声明符被读成一条**嵌套的声明**。整个文件随后级联：`winnt.h`
的 417 条错从这里开始。

**成因**：规则读**一个**声明符就要求 `;`。而 `typedef` 是声明说明符，后面跟的是普通的
**init-declarator-list**——C/C++ 头文件到处都是"一行引入好几个名字"。

**性质**：缺规则（少一层循环）。**两个半边**：parser 要读这个列表，并且**每个声明符各自的名字**都要记成类型名
（否则 `LPWCH q;` 后面读不成声明）；`sema::scopes` 也要**绑定每一个**声明符（它原来只取第一个
`first_child(node, Declarator)`），否则第二个名字的事实根本不存在。

**护栏**：`gaps.rs::a_typedef_declares_every_name_it_lists`（第二个名字也能当类型用）+
`tests/scopes.rs::a_typedef_declares_every_name_it_lists`（三个名字都是 `BindingKind::Typedef`）。

### B46. 类头里的宏（`struct DECLSPEC_ALIGN (8) _NAME {`）—— 已修复

```cpp
typedef struct DECLSPEC_ALIGN (8) _XSAVE_AREA_HEADER { … } XSAVE_AREA_HEADER, *PXSAVE_AREA_HEADER;
```

**现象**：`DECLSPEC_ALIGN` 被当成 tag 名，`(8)` 报 `expected ;`，之后的对齐结构全部级联——`winnt.h` 剩下的
5 层嵌套 typedef 都是它。

**成因**：类头只认"关键字 → 名字 → `{`"，而编译器自己的对齐属性宏就写在关键字与名字之间。宏定义在
`_mingw.h`（**另一个文件**），所以文件局部的宏表不可能知道它，拼写也不是证据（第 16 条）。

**性质**：缺规则，但**接受它不花任何代价**：类关键字之后语法只允许属性、名字、`{`、`:`、`;`，而"名字 + 括号组"
一个都不是——没有第二种读法可以被抢走。守卫是**后面跟什么**（名字 / `{` / `:` / `;` 才算），所以真错误仍然是错误。
和 `namespace std _GLIBCXX_VISIBILITY(default) {` 用的是同一条形状判据（`eat_namespace_head_macros`）。

**护栏**：`gaps.rs::a_class_head_may_carry_a_macro_before_its_name`。

### B47. 条件里声明一个变量（`if (Foo p = get())`）—— 已修复

```cpp
if (int n = g()) { }
if (const auto n = g()) { }
while (const size_t n = len()) { }
```

**现象**：三种写法、两种症状，其中一种是**静默**的：

```text
if (Foo* p = get())      读成 `Foo * p = get()` —— BinaryExpr，**一个诊断都没有**，而它不是合法 C++
if (Foo p = get())       `expected ), but get identifier` 打在 `=` 上
if (const auto n = g())  `expected primary expression` 打在 `=` 上
```

**成因**：条件的声明形式走的还是 `parse_declaration`，而那条规则以 `expect_semicolon` 结尾——条件里没有 `;`，
于是尝试失败、`parse_condition` 回退到**表达式**读法。实测：`bits/ranges_algo.h:3326`、`bits/ranges_algobase.h:140`。

**性质**：缺规则。修法是 `parse_condition_declaration`：与 `for` 头部的声明同形（specifiers + 一个
init-declarator，没有自己的 `;`），外加两条标准要求——**恰好一个**变量，而且**必须有初始化式**。第二条就是判据
本身：`if (a && b)` 同样是"名字 + `&&` + 名字"，没有它就会被读成"`b` 声明为 `a&&`"，那是错答案而不是缺答案
（第一版就是这么写的，`gaps.rs` 的负例把它挡回来了）。

**护栏**：`gaps.rs::a_condition_may_declare_a_variable`——读得出，条件是 `Declaration`，而
`if (v.size())` / `if (i++)` / `if (a && b)` 仍然**不是**。

### B48. 分配式的括号组是初始化式，不是参数表（`new T(a, *q)`）—— 已修复

```cpp
::new ((void*)__ptr) _Tp(allocator_arg, *__a._M_a, std::forward<_Args>(__args)...);
```

**现象**：`expected a type specifier` 打在括号组里那个 `*` 上。三个文件：`bits/uses_allocator.h`、
`bits/node_handle.h`、`memory_resource.h`。

**成因**：`a_parameter_list_is_the_type` 只看括号组的**第一个** token——`a` 是名字，于是整组被判成参数表，
`*q` 没有类型就报错。参数是"类型 + 声明符"，所以只有**每个**元素都以类型开头，这组才可能是参数表。

**性质**：缺规则（判据不够精确），改的是判据本身而不是新开分支，所以 `void (int)`、`new (int(*)(int))()`
这些真参数表的读法一个字没动。

**护栏**：`gaps.rs::an_allocation_initialiser_is_not_a_parameter_list`，里面同时登记了两个**同族但本来就**
读不出的形状（`new T(*q)`、`new (Widget)(1)` 当初始化式）——写在 `assert_does_not_read_yet` 里，而不是假装支持。

### B49. `__typeof__` / `__decltype` 就是那个关键字 —— 已修复

```cpp
typedef __typeof__(nullptr) nullptr_t;
typedef __decltype(__comp) _Cmp;
```

**现象**：`expected ;` 打在名字上——`__typeof__` 被读成**声明符**（名字 + 参数表），`T2` 于是成了多余的东西。

**成因**：lexer 不认这三个拼写（`__typeof__` / `__typeof` / `__decltype`），它们以**标识符**身份进入语法，
而"`decltype` 是类型说明符"那一整套规则（说明符序列、声明锚点、`a_decltype_here_is_a_type`）因此都没轮到它们。

**性质**：缺规则，改动一行：`name_to_kind` 把三个拼写映射到 `DecltypeKeyword`。树保留原文（token 文本就是源码
文本），所以需要区分拼写的消费者读文本即可，而问"这是不是一个类型表达式"的消费者两种拼写都得到对的答案。

**护栏**：`gaps.rs::the_gnu_spellings_of_decltype_are_that_keyword`（四种拼写都读得出，且 lexer 给出同一个 kind）。

### B50. 类头里名字**之前**的裸宏（`class _GLIBCXX17_DEPRECATED unary_negate`）—— 已修复

```cpp
template<typename _Predicate>
  class _GLIBCXX17_DEPRECATED unary_negate        // bits/stl_function.h:1021
  : public unary_function<typename _Predicate::argument_type, bool>
  { … };
```

**现象**：`expected ;` 打在**下一行**的 `:` 上——类头被读成"一个叫 `_GLIBCXX17_DEPRECATED` 的类"，
于是 `unary_negate` 成了多余的东西，基类子句那行看起来才是错的。

**成因**：B46 那条规则只认"宏 + **括号组** + 名字"（`DECLSPEC_ALIGN (8) _NAME`），而弃用标记是**裸的**宏，
没有括号组。

**性质**：缺规则，判据是"两个名字连排"——类关键字之后语法只允许一个属性、一个名字、`{`、`:`、`;`，
所以第一个名字必然是宏。两道守卫同时要，缺一条就误读：

```text
第二个名字后面必须接得上类头（{ / : / ; / 指令）   否则 struct S requires C<T> { } 会被读成"类名 requires"
final / override / requires 是**拼写**不是 token     否则 class A final : B 会被读成"类名 final"
```

（后一道是被 `concepts.rs` 的两条测试挡回来的——第一版没有它，`requires` 子句当场被吃掉。）

**护栏**：`gaps.rs::a_class_head_may_be_written_in_pieces`（读得出 + 类名确实是名字 + `class A final` 保持原读法）。

### B51. 类名与基类子句之间的指令（`class move_iterator` / `#ifdef` / `: public …` / `#endif`）—— 已修复

```cpp
template<typename _Iterator>
  class move_iterator                              // bits/stl_iterator.h:1435
#ifdef __glibcxx_ranges
    : public __detail::__move_iter_cat<_Iterator>
#endif
  { … };
```

**现象**：同样是 `expected ;` 打在 `: public …` 那一行上。

**成因**：类头规则只在**游标处**看 `:`，而名字之后是一个 `#`——于是类头在名字处结束，声明去找 `;`。

**性质**：缺规则，接缝位置（与 §2.1 那九处接缝、B40 的 `try` 同族）：`parse_class_like_head` 里把
"读指令 → 再看基类子句"做成一个循环，两个顺序都覆盖（基类子句写在分支里，或整个类头分两个分支写）。
指令按节点读进来，树保持无损，两个分支都还在。

**护栏**：`gaps.rs::a_class_head_may_be_written_in_pieces`（同一组断言里的两条）。

### B52. 函数式转换没有实参（`int()`、`typename T::type()`）—— 已修复

```cpp
__iterator_category(const _Iter&)
{ return typename iterator_traits<_Iter>::iterator_category(); }     // bits/stl_iterator_base_types.h:242
return int();                                                      // 同一个洞的另一半
```

**现象**：`expected primary expression` 打在 `int` / `typename` 上，整个 `return` 语句跟着走。

**成因**：函数式转换的实参表用的是 `parse_parenthesized_expression`，而它读的是**表达式**——`)` 不是表达式，
于是"两臂"（关键字类型那一臂与 `typename` 那一臂）都拿不到 payload，整条表达式失败。空实参表恰恰是最常见的
函数式转换（默认构造的临时量）。

**性质**：缺规则，一条：两臂共用一个 `parse_functional_payload`——空 `()` 也读成一个**没有子节点的 `ParenExpr`**
（"没有实参的调用"就是它），`{}` 仍走既有的花括号初始化式。负例是另一半：`f()` 是 **call**（`IdentifierExpr`），
不是 `CastExpr`；裸的 `typename T::type`（没有 payload）仍然不是表达式。

**护栏**：`gaps.rs::a_functional_conversion_may_have_no_arguments`（读得出 + `int()`/`typename A::type()` 是
`CastExpr` 而 `f()` 是 `IdentifierExpr`）。

### B53. 说明符被 `#if`/`#else` 切开、且 `#else` 那一支以**名字**结尾 —— 已修复

```cpp
      template<typename _Tp>
#if X
      static void                                  // 这一支只有关键字
#else
      static C                                     // 这一支以**名字**结尾（真实文件里是 __enable_if_t<…>）
#endif
      f() { }                                      // 函数**定义**（带函数体）
```

**现象**：`expected ;` 打在**下一个成员**的 `template` 行上——`bits/alloc_traits.h:430-438` 的那个成员本身是好的，
却让 454 行（下一个成员）成了整个文件的首错，中间二十多行看起来都没问题。

**成因（这是实测出来的，和本条第一版写的不是一回事）**：第一版写的是"函数自己的名字被当成类型的一个词"，把树打出来
之后看到的是**反方向**：

```text
DeclSpecifierSeq   static  void  #else  static      ← 两支被并进同一个说明符序列，而 `void` 已经"命名了类型"
  InitDeclarator   Declarator(NameExpr `C`)          ← `C` 成了**声明符名**
PreprocessorDirective `#endif`
Declaration        TemplateType(`f`)  `(` `)` `{` `}`  ← 真正的成员，整块成了瓦砾
```

`void` 在**另一支**里，而"这个序列已经命名过一个类型"这个判断跨过了分支边界——于是 `#else` 那一支的
`__enable_if_t<…>`（它才是**本支**的类型）被拒绝在类型位置之外，成了声明符；声明在 `#endif` 处没有 `;` 就结束了。
诊断因此落在下一个成员上（维护约定第 13 条：一个缺陷会遮住另一个，而且**报到别处**）。

**性质**：缺规则（判据的**作用域**不对，不是判据错）。修法是把"这一支知道什么"在分支边界上重置——
`#else`/`#elif` 之后把三样东西恢复成循环开始时的样子：本支还没命名过类型（`has_type_specifier = false`）、
"再收一个名字"的许可没花掉（`name_allowed = allow_second_name`）、判据回读事件流的**窗口**挪到指令之后
（`specifiers_from`，它决定 `a_further_name_may_join` 与 `a_class_definition_was_written` 看得见哪一支）。
`#endif` **刻意不是**这种边界：它之后声明继续写的是两支**共同**的部分，那正是结尾需要的知识。

**护栏**：`gaps.rs::a_split_member_head_may_name_its_type_in_the_else_branch`——十个读得出的变体（含真实文件那段
带 `requires` 与尾随 `noexcept` 的拼写、"分支不命名类型、类型留在尾巴上"、"两支各自命名类型"、"`#elif`"），
外加三条**形状**断言（声明符的名字是 `f`、类体只有一个成员、两个指令都在类型自己的节点里）。同一条里还钉着一个
**同族但更窄的已知缺漏**：分支以**第二个**名字结尾（`static MY_API C`）仍然读不出——修它需要"名字后面跟着指令
就不是声明符名"这条证据，而那条证据会撞上"初始化式写在两个分支里"这个今天读得出（`static const int n` +
`#if X = 1; #else = 2; #endif`）的形状，所以先记着不修。

**量到的**（`docs/std-library.md` 里有整轮的账）：128 个文件的闭包 消息 802 → **796**，`alloc_traits.h` 的首错
从 454 行推到 **536** 行（本条的成员本身开始读了）；455 个文件的分析闭包 2359 → **2353**。

### B54. 模板实参是表达式时，**它后面的那个逗号**被吃掉了 —— 已修复

```cpp
  X<!C<T>, bool> f;                        // __enable_if_t<!__has_construct<…>, bool>，alloc_traits.h:530
  S<3, 4> x;                               // 同一个洞最安静的拼写
  S<3, long> x;                            // 同一个洞最响的拼写
```

**现象**：三种表现，按安静程度排：

```text
S<3, 4> x;              完全无诊断 —— 读成**一个**实参 `(3, 4)`（逗号运算符）
S<3, long> x;           expected primary expression 打在 `long` 上（类型没有表达式读法）
X<!C<T>, bool> f;       实参在逗号处结束、右边是空的，`bool`、`>`、名字、函数体全成瓦砾
```

**成因**：模板实参的读取器先试类型、失败后回落到**表达式**，而它调的是 `parse_expr`——**含逗号运算符**的那一级。
分隔实参的逗号于是被当成了运算符。`array<int, 3>`、`Grid<T, 3>::fill` 这些拼写之所以看不出问题，是因为逗号落在
**类型**读法已经接受的类型**后面**，回落根本没跑。

**性质**：缺规则，一条改动（回落改调 `parse_assignment_expr`）。它的位置**早就写在文档里**了：`exprs.rs` 的
`Level` 表列着"自己拼分隔符的规则读一个元素"，其中一行就是 `types::parse_template_argument (expression fallback)`——
**文档说这条已经修过，代码却还停在旧读法上**，因为当时验证用的例子（`Grid<T, 3>::fill`）走的是类型读法。
这是本项目里少见的"文档与代码不一致"的实例，值得记下来：那张表的用法是"改共享入口之前先把使用者列全"，
所以它写下的每一条都得能被检查——`cargo test` 全绿并不证明任何一条还在生效。

**修完之后把那张表逐行查了一遍**（方法是 grep 每条规则实际调用的读取器）：七行里其余六行都对——
调用实参用 `parse_argument`、`parse_expression_list` / `parse_initializer_clause` / `parse_capture` /
位域宽度 / 模板参数默认值六处都用 `parse_assignment_expr`。这次逐行检查的**做法**已经写进 `exprs.rs` 那张表的
文档里（"Checking the table"），并要求**新增一行之前**先照这个办法查一次，而不是等普查撞出缺陷再回头补。

**护栏**：`gaps.rs::a_template_argument_read_as_an_expression_stops_at_the_comma`——十三个读得出的拼写
（含 `integer_sequence<int, 0, 1, 2>`、`S<(a, b), 2>`）＋**计数**断言（`S<3, 4>` 是两个实参，`S<(a, b)>` 是一个，
因为括号让逗号重新成为运算符）＋"每个实参有自己的字面量节点"。计数那一半是必须的：安静的那一种拼写没有任何诊断。

**量到的**：128 个文件的闭包 消息 796 → **721**、455 个文件的分析闭包 2353 → **2278**（两份都是 −75，方向一致，
因为库文件同时属于两份清单）；`alloc_traits.h` 的首错从 536 行推到 **689** 行。干净数两份都没动——它是
"把首错往后推"那一类，也正是这一步把 B55 那个形状（689 行之后）露了出来。

### B55. 条件决定的是**限定符**（`noexcept` 写在两个分支里）—— 已修复

```cpp
      template<typename _Up, typename... _Args>
	construct(allocator_type& __a, _Up* __p, _Args&&... __args)      // bits/alloc_traits.h:662
#if __cplusplus <= 201703L
	noexcept(noexcept(__a.construct(__p, std::forward<_Args>(__args)...)))
#else
	noexcept(__is_nothrow_new_constructible<_Up, _Args...>)
#endif
	{ … }
```

**现象**：诊断出现在**二十七行之后**的一个无关成员上（`alloc_traits.h:689` 的 `template<typename _Up>`），
而 662 行那个成员在只有一个分支的拼写下**完全不报错**。

**成因**：声明符的后缀读取器（`noexcept`、`const`、`-> T`、宏后缀）在**指令循环之前**就跑完了；指令循环
（`finish_init_declarator` 里"指令与宏交替"的那个 loop）读了 `#if` 之后只再问宏后缀，于是 `noexcept` 留在原处，
被**下一个声明**读成了它的类型：

```text
Declaration@11..28   void f()           ← 第一个声明在这里结束，`#if` 是它的孩子
Declaration@28..80   BuiltinType over `noexcept` `(true)`   ← `noexcept(true)` 成了"类型"（两个 token 毫无关系）
                     InitDeclarator  { }  ← 后面那个成员成了用 `{ }` 初始化的变量
```

**性质**：缺规则（同一条缝的第三次：指令落在"参数表与函数体之间"，而**限定符正是写在那里**的）。修法是读完一条
指令后**再问一次限定符**——和旁边那行"再问一次宏后缀"是同一条理由：条件能决定后缀里的**任何**一部分。

**护栏**：`gaps.rs::a_conditional_may_decide_a_functions_qualifier`——九个读得出的拼写（`noexcept`、`const`、
`-> T`、单分支、条件头、宏后缀）＋三条**形状**断言（`{ }` 是函数体而不是初始化式；类体只有一个成员；成员只有
一个 `DeclSpecifierSeq`）。三条断言都是为了那个**静默**的读法：单分支拼写旧读法**没有任何诊断**，
`assert_reads` 会说它是对的。

**量到的**：`bits/basic_string.h`（`4532` 行那条首错）因此整个文件变干净——两份清单各 **+1 个干净文件**；
128 个文件的闭包 40 个失败、消息 721 → **710**；455 个文件的分析闭包 112 个失败、2278 → **2267**。

### B56. 花括号函数式转换（`int{}`）与作模板实参的 `T{…}` —— 已修复

```cpp
  auto a = int{};                          // 表达式位置：函数式转换的**花括号**拼写
  g(int{});  return int{};                 // 同一个洞，另外两个位置
  X<int{}> m;                              // 模板实参：**静默**读成比较式 `X < int{} > m`
  X<size_t{}> m;  X<A{1, 2}> m;            // 名字做类型时同样静默
  using A = X<int{}>;                      // 别名目标
  template<typename T> struct Q<T, int{}> { };                    // 偏特化的名字
  struct X<A, void_t<decltype(h(size_t{}))>> : B { };             // 类头：基类子句与类体成瓦砾
  bits/alloc_traits.h:941（`__void_t<…, decltype(…allocate(size_t{}))>>`）的首错就是最后两条
```

**现象**：同一个构造的两种结局，按**能否看见**排：

```text
auto a = int{};               expected primary expression 打在关键字上（关键字类型没有表达式读法）
X<int{}> m;                   **零诊断**：整条声明读成 `BinaryExpr((X < int{}) > m)` —— A0 类
X<size_t{}> m;                同上（名字做类型时，类型读法停在 `{`，而**那个停被当成了实参的结束**）
using A = X<int{}>;           `X` 成了整个类型，`<…>` 是瓦砾
struct Q<T, int{}> { };       类名读成裸 `Q`，实参是瓦砾
struct X<A, void_t<…>> : B { } 类头被判成"没有类体"，基类子句与类体都是瓦砾
```

**成因**：一个想法**五处**漏了同一件事——"**配对的括号属于包着它的东西**"。前三处是**数角度的扫描**，
它们的停表里有 `{`/`}`（B30 那条"三处数角度的扫描要共用 `angle_depth_delta`"的教训在括号上又出现了一次），
于是遇到 `X<int{}>` 就回答"这个 `<` 是比较"；
第四处是模板实参读取器：类型读法在 `{` 处停下，而"类型读法成功了"被当成"实参读完了"；第五处是
`parse_primary_expr` 的函数式转换分支，它的守卫只认 `(`，所以 `int{}` 根本没有表达式读法。

**性质**：缺规则（同一判据的第五种用法），改动是五处对称的小改：三处扫描加 `braces` 计数（**不成对的**
`}` 仍然结束扫描，那是外层自己的括号）；实参读取器在"类型后面跟着 `{`"时改走表达式读法；函数式转换分支的
守卫接受 `{`。修 `int{}` 那一处时必须同时看住 `int(x);`——它**仍然**是声明（括号里是声明符名），
这也是原来那道守卫的理由，现在由声明读法自己"没有名字就没有初始化式"的拒绝来保证。

**护栏**：`gaps.rs::a_braced_conversion_is_a_value_not_a_type`——二十三个读得出的拼写（含真实文件的整行、
三处扫描各自的形状、`int{1}`、`bool{true}`）＋**形状**断言：`X<int{}> m;` 里**没有** `ExpressionStat`、
声明符名是 `m`、实参是**一个**；类头有 `ClassBody` 且实参是两个；`int{}` 是 `CastExpr` 而 `int(x);` 仍是
声明。这些断言都是冲着**没有诊断的那一半**去的：修之前 `assert_reads` 会说 `X<int{}> m;` 是对的。

**量到的**：128 个文件的闭包 消息 710 → **702**、455 个文件的分析闭包 2267 → **2259**（各 −8，干净数没动）；
`alloc_traits.h` 的首错从 941 行推到**文件最后一行**（1053 行的 `#endif`）——而它推出来的正是 B57。

### B57. requires 表达式里的指令（`= requires (T t) { #if … #endif };`）—— 已修复

```cpp
  template<typename _Tp, typename... _Args>
    static constexpr bool __can_construct_at                   // bits/alloc_traits.h:140，逐字缩下来的一半
      = requires (_Tp* __p, _Args&&... __args) {
#if __cpp_constexpr_dynamic_alloc
        std::construct_at(__p, std::forward<_Args>(__args)...);
#else
        ::new((void*)__p) _Tp(std::forward<_Args>(__args)...);
#endif
      };
```

**现象**：`unexpected token` 打在成员的 `};` 上；在 `bits/alloc_traits.h` 里那条报在**文件最后一行**，
而树里 `template` / `<` / `>` / `static` … 全是 `ErrorNode`，**类体在成员的 `};` 处提前关闭**——后面的成员
（包括 `__is_allocator` 那些）都掉到外层作用域，所以整份文件只剩一个诊断，而且它离成因 900 行。

**成因**：require-seq 的读取循环是"一条要求一个 `;`"，而**要求位置上出现 `#` 时没有任何读法**，于是整个
requires 表达式失败。这一族（"指令落在构造的接缝上"）在 §2.1 里已经修过九处，**没有一处落在 requires 体内**——
它是这一族的**最后一处**。

**性质**：缺规则，一处接缝：要求位置上读到 `#` 就按节点读进来、再问下一个 token，和
`parse_braced_initializer` 在元素之间做的是同一件事（`#` 在这个位置上不可能是别的：要求以表达式、`typename`、
`{`、嵌套 `requires` 开头，或者以指令开头）。

**护栏**：`gaps.rs::a_conditional_may_decide_a_requirement`——九个读得出的拼写（单分支、两分支、两段并列、
类型要求与复合要求各写一支、`#if` 前后各有一条要求、带模板头、整份成员按分支写一遍）＋**形状**断言：
`RequiresExpr` 里有**两条** `Requirement` 和**三条**指令节点，而且带条件的那个成员与它后面那个成员
**都还是成员**（`direct_members == 2`）。形状断言是主要的：读坏时的样子是"体被放弃、token 成瓦砾"，
普查里**每个数字都不变**。

**一条刻意不修的拼写**（拿 g++ 验过）：整份成员按分支写、而 `#endif` 与类的 `};` **共用一行**——
`#endif };` 里 `};` 是指令行上的多余 token，g++ 报 `warning: extra tokens at end of '#endif' directive` 加
`error: expected '}' at end of input`，与本 parser 的抱怨相同。所以那是**写错了的代码**，不是缺规则
（维护约定第 36 条：片段先拿编译器验一遍）。测试里因此只留一条注释，不留用例。

**量到的**：`bits/alloc_traits.h` **整个文件变干净**——两份清单各 **+1 个干净文件**（128：88 → **89**，
455：343 → **344**），消息 702 → **701** / 2259 → **2258**。至此那条链走完了：
B53→B54→B55→B56→B57 把该文件的首错从 454 一路推到文件最后一行，然后推没了。

### B58. 坏掉的成员/语句仍然会把类体、块、链接块一起带走（恢复）—— 已修复

```cpp
struct Probe {
  static constexpr bool ok = requires (T t) {
#if X
    t.f()                       // 要求被指令切成两半：`;` 在另一个分支里
#endif
    ;
  };
  int after;                    // ← 修之前读在**文件作用域**，不是类里
};
```

**现象**：坏掉的那个成员/语句之后的所有东西都掉到外层——类体（`after` 不再是成员）、函数块（`after();` 不在
函数里）、链接块（`extern "C" { … }` 后面的声明跑出去）。在 `bits/alloc_traits.h` 里它的表现是"整份文件只有
一个诊断，而且离成因 900 行"（见 B57）；在 MinGW 的头里它的表现是链接块被撑到文件末尾。

**成因（读法之外的第二个机制）**：把失败成员的 marker **带结束事件**关掉
（`end_marks_to`，维护约定第 34 条）决定的是**树**长什么样，它**不能把已经吃下去的 token 吐回来**。
一个失败时已经吃掉 `{` 却没有对应 `}` 的成员/语句（requires 体、函数体、块、花括号初始化式都是这个形状）
于是让外层容器少一个 `}`，而恢复是"跳到下一个 `}`"——那个 `}` 属于**失败的那个构造**，容器却把它当成了自己的
结束。B57 的接缝让 `alloc_traits.h` 读通了，这个机制本身没被动过。

**性质**：**恢复问题**，不是缺规则（所以与 B57 分开记、分开量）。修法是**花括号债**（brace debt）：失败之后
把这个构造**消费掉却没有配对的 `{`** 记成欠账，容器在还清之前不许结束，用来还账的 `}` 读成 `ErrorNode`
（它是没人认领的 token，正是 error node 的语义）。三个容器各一份：

```text
类体        parse_class_body_members   成员失败 → 记账
语句块      parse_stats               语句失败 → 记账（含"跳到 `;`/`}`"那一段里跨过的 `{`）
链接块      parse_linkage_block       声明失败 → 记账（MinGW 头全靠它）
```

**两条路径都要记**，这是这一条最容易只修一半的地方：失败可能**停在成员中间**（事件流里还留着，用
`CppParser::brace_balance_since` 读），也可能**整块回滚**（声明/表达式那次二选一试探会 `rollback`，事件被截断）
——回滚之后循环会把成员一个 token 一个 token 地当瓦砾再读一遍，那个 `{` 就在这一遍里被记上。

**护栏**：三个容器各一处，都问"后面那个东西还是不是成员/语句"而不是数瓦砾的个数：
`gaps.rs::a_failed_member_with_an_unbalanced_brace_keeps_the_members_after_it_members`（requires 体、
函数体、缺 `;` 三种形状）、`a_statement_the_parser_gives_up_on_keeps_the_block_after_it`（新加三种：
花括号初始化式、lambda 体、没关的嵌套块）、`a_linkage_block_keeps_the_directives_written_inside_it`
（链接块里那个坏声明）。

**量到的**（这一条是目前单次收益最大的一次）：

```text
                        128 个文件的闭包              455 个文件的分析闭包
                        干净  报错  消息              干净  报错  消息
修之前                   89    39   701               344   111  2258
类体 + 语句块            92    36   375               349   106  1305
+ 链接块                 92    36   375               364    91   688
```

128 那份是 **+3 个干净文件、−326 条消息**；455 那份（含 MinGW 头，链接块最密的地方）是
**+20 个干净文件、−1570 条消息**。三个文件从"报错"直接变干净（`functional_hash.h`、`stl_vector.h`、
另一个 `alloc_traits.h`），另有四个文件的首错往后跳了一大段（`predefined_ops.h` 65 → 80、
`stl_iterator.h` 1633 → 3091、`stl_tree.h` 1086 → 2468、`compare` 568 → 672）。这也说明**为什么它值这么多**：
一个坏成员带走整个类之后，那个类里的每个成员都会各自再报一次错——级联的账在这里。

### B59. `typedef` 声明符**后面**的属性（`typedef int v4 __attribute__ ((…));`）—— 已修复

```c
typedef int __v4si_u __attribute__ ((__vector_size__ (16), __may_alias__, __aligned__ (1)));
typedef short __v32hi __attribute__ ((__vector_size__ (64)));      // avx512bwintrin.h:361
typedef double __v8df __attribute__ ((__vector_size__ (64)));      // avx512fintrin.h:3817
typedef int v4 [[deprecated]];                                    // 标准拼写，同一个位置
typedef int __v4si_u __attribute__ ((__vector_size__ (16),\       // 真的文件里带 `\` 折行
                                     __may_alias__, __aligned__ (1)));
```

**现象**：`expected ;` 打在**声明符自己的名字**上——属性整块成瓦砾，声明在名字之后就结束了。
闭包里有 **8 个文件**（全是 GCC 的 `*intrin.h` 类型动物园）的首错是这个形状。

**成因**：普通声明路径早就读这个位置的属性了（`finish_init_declarator` 里那一段，
`int x [[maybe_unused]] = 1;`），而 `typedef` 是**第二条路径**——它自己拼声明符循环、根本不经过
`finish_init_declarator`，于是漏了同一件事。两种拼写（`[[…]]` 与 `__attribute__((…))`）共用一条规则，
所以修法是**一次调用**而不是每个编译器一次。

**性质**：缺规则（同一条判据的第二种用法，维护约定第 14 条那句话的又一例）。位置与普通声明一致：
声明符之后、初始化式之前。折行（`\`）不是问题——`LineContinuation` 在声明里本来就通（本轮另外验过七种
折行形状）。

**护栏**：`gaps.rs::a_typedef_may_carry_an_attribute`——九个读得出的拼写（含真实文件那一行与折行版、
两种拼写、属性在类型**之前**、多声明符的 `typedef`、变量与函数上的属性）＋**形状**断言：
属性是 `TypedefDecl` 自己的节点，而且它声明的名字之后能当类型用。

### B60. 条件决定的是**属性**（模板头与声明之间，`#if [[attr]] #endif`）—— 已修复

```cpp
template<typename _Ex>                              // bits/nested_exception.h:203
# if ! __cpp_rtti
  [[__gnu__::__always_inline__]]
#endif
  inline void
  rethrow_if_nested(const _Ex& __ex)
```

**现象**：整条声明失败，**文件只剩一串裸 token**，诊断打在属性那一行（`expected a type specifier`）。

**成因**：模板头与声明之间的属性**已经有规则**（"那个位置既不属于头也不属于说明符序列，所以由声明规则
自己读"）。缺的是**指令**：说明符序列只在**两个说明符之间**读指令（B53 那条），而这里 `#` 出现在
**第一个**说明符之前——按设计那是"调用方该读的"，可调用方（声明规则）在这个位置只读了一次属性就把手
交给了说明符序列。于是 `#endif` 落在序列期待第一个说明符的地方，整条声明失败。

**性质**：缺规则，与 `finish_init_declarator` 里"指令与宏后缀交替"是同一处修法、早一个位置：
在模板头之后把**属性与指令交替**读到都不在为止（两种顺序都覆盖）。

**护栏**：`gaps.rs::a_conditional_may_decide_an_attribute`——八个读得出的拼写（两种顺序、
`attr #if attr #endif`、真实文件那一行、无条件的属性、无属性的条件、普通声明开头的属性）＋**形状**断言：
属性与模板头在**同一条** `Declaration` 里。反面是"普通声明开头的属性仍然是说明符"和"没有模板头时指令仍由
调用方读"。

**量到的**（B59+B60 一起）：

```text
                        128 个文件的闭包              455 个文件的分析闭包
                        干净  报错  消息              干净  报错  消息
修之前                   92    36   375               364    91   688
B59 + B60                92    36   374               370    85   609
```

455 那份 **+6 个干净文件、−79 条消息**（`avx512bw/cd/f/vlbw/vl`、`avxintrin`、`emmintrin`、`mmintrin`、
`nested_exception.h` 这些从首错变干净或首错后移）。八个 `*intrin.h` 的首错一起消失，是这一轮除 B58 之外
最集中的一次。

### B61. 编译器自己的类型拼写（`__int128` / `_Float16` / `__int64`）与**方言** —— 已修复

```cpp
unsigned __int128 x;                       // 修之前：读成"名为 __int128 的变量，后缀是宏 x"——**零诊断**
void f() { auto r = (unsigned __int128) 1; }   // expected primary expression
using T = unsigned __int128;               // expected `;`（类型读到 `unsigned` 就结束）
typedef unsigned __int128 u128;
void f() { _Float16 h = 1; }               // 声明失败：名字 `h` 被并进类型
bits/bmi2intrin.h:86 与 bits/ranges_base.h:86 的首错都是它
```

**成因**：`__int128` 在词法上是标识符，而在**类型 id**（没有声明符收尾的位置）里 `allow_second_name` 是 false，
所以"限定符 + 保留拼写"被拒在类型之外。更糟的是声明里那一种读法**不报错**：`unsigned __int128 x;` 读成
`Declaration[DeclSpecifierSeq[unsigned] InitDeclarator[__int128] MacroCall[x]]`——一个叫 `__int128` 的变量，
后缀是一个叫 `x` 的宏。这正是本文档开篇的 A0 类：树良构、无损、**零诊断**。

**性质**：缺规则，但答案**取决于目标编译器**——所以修法不是加长拼写表，而是先有"方言"这个配置：

```text
                        GNU（g++ / clang++）      MSVC（cl.exe）
__int128                内建类型                  根本不是类型
__int64                 不是类型（MinGW 用           内建类型
                        `#define __int64 long long`）
_Float16 / __bf16       内建类型（GCC）            没有这个拼写
```

`cpp_parser` 因此多了一个 [`Dialect`]（`Gnu` / `Msvc`，默认 `Gnu`），`ParserConfig::with_dialect` 传进去，
语法层只有**一处**读它（`a_type_the_compiler_spells`）：命中的拼写读成 `BuiltinType` 说明符（拼写留在文本里，
和 `__forceinline` → `InlineSpec` 是同一个安排）。**走普通说明符规则而不是"编译器关键字跳过"那条路**，
是因为"产出了一个 `BuiltinType`"正是 `has_type_specifier` 的判据——不然 `_Float16 h = 1;` 里 `h` 会被并进类型。

**方言是配置，不是猜测**：`Session::open` 从工具链自己 `-dM -E` 吐出的预定义宏里读 `__GNUC__` / `_MSC_VER`
（`Dialect::from_predefined_macros`，`clang-cl` 两个都定义 → 按 MSVC 拼写），放进 `CompilerConfig`；
**它同时进 `context_hash`**——同一段文字为两个目标读出的就是两份摘要，缓存把两者混起来就是给出错答案。

**量到的**：128 个文件的闭包 374 → **372**；455 个文件的闭包 370 → **371 干净**、85 → **84 报错**、609 → **607**。
`bits/bmi2intrin.h` 整个文件变干净，`bits/ranges_base.h` 的首错从 86 行（`__int128` 参数）推到 214 行。
另有**一处静默错树被修掉**，而它一个数都不占：`unsigned __int128 x;` 现在声明的是 `x`。

**两条被量下来的取舍**（都写进测试/文档而不是悄悄放过）：

1. **`_mingw.h:248` 的 `typedef int __int128 __attribute__ ((__mode__ (TI)));`**（`#ifndef __SIZEOF_INT128__`
   那一支，只有没 `__int128` 的编译器才走）在 GNU 方言下没有声明符名——**它读成一个不声明任何东西的
   `typedef`，而这是静默的**。想过让它报错（"`typedef` 必须命名东西"），实测**代价 9 个干净文件、246 条消息**
   （那类形状在真实头文件里到处都是），所以**不报**：那一支在 GNU 目标下是死代码，而按 MSVC 方言读它就完全正确。
   这一条因此是"方言"这个概念要付的价，记在这里。
2. **`Dialect::Gnu` 是默认值**：本项目的语料是 GNU 编的，而且歧义不对称——把 `__int128` 读成名字是静默错树，
   把 `__int64` 读成名字只是保守（让它成为类型的那个 `#define` 是指令，不是这一层的事）。

**护栏**：`gaps.rs::a_type_may_be_spelled_by_the_compiler`——十二个 GNU 读得出的拼写（声明、cast、别名、
typedef、成员、形参、`_Float16`/`__bf16`/`__float128`，以及**不是**类型的 `__restrict`/`__extension__`）＋
**形状**断言（两个 `BuiltinType`、**没有** `MacroCall`、声明符名是 `x`）＋ MSVC 侧两面（`__int64` 是类型、
`__int128` 是名字、`typedef int __int64;` 在 GNU 下仍然读得出）。分析层两处：`store.rs` 的
`the_key_knows_which_compiler_the_file_is_read_for`（方言进键）与 `index/mod.rs` 的
`a_summary_is_read_for_the_compiler_it_was_configured_with`（方言真的传到了 parser：同一段文字在两种方言下摘要不同）。

**顺带记一条方法论**：`Dialect` 与 `CppLanguageLevel` 是**两个问题**——`-std=gnu++20` 同时是 C++20 *和*
GNU 拼写，而 `CppLanguageLevel::GnuCpp` 是一个"级别"，说不出来。级别管特性（raw string、`<=>`），方言管
"编译器保留的名字是什么意思"。两者混在一起是历史，分开是这一步。

```cpp
void f() try { } catch (...) { }        // `try` 写在声明符与函数体之间
S::S() try : m(1) { } catch (...) { }   // 构造函数版本，构造函数初始化列表在 `try` 之前
void f() try { } catch (X& x) { }       // 处理器可以带参数
```

**现象**：四个报错，第一个落在 `void` 上（`expected primary expression`），然后是 `{`、`catch`、结尾的 `}`。
整条定义读不成函数定义。

**成因**：`try` 作为**语句**有规则（所以同样的 token 写在函数体**里**完全正常），但"声明符与函数体之间"
这个位置没接：`parse_a_definition_per_branch` 读完声明符就等 `{`，`try` 不在它的 follower 里。

**性质**：缺规则，接缝位置——和维护约定第 31 条同族（"向回走的判据要知道什么包着这个构造"）
以及 B40（`try` 的两个 `#if` 接缝，已修）是同一处构造的第三、第四种写法。

**护栏**：`gaps.rs::constructs_the_parser_does_not_read_yet`（`Where::File`）钉住它现在读不出来，
能力落地那天这条会失败——那正是有意为之。

### B62. `T f(U) { … }`：**花括号体**说明那个括号组是形参表 —— 已修复

```cpp
_GLIBCXX20_CONSTEXPR
inline _Iter_less_val
__iter_comp_val(_Iter_less_iter)                  // bits/predefined_ops.h:79
{ return _Iter_less_val(); }                      // 首错：a declarator takes only one initializer

template<typename _Ex>
  __attribute__ ((__always_inline__))
  inline exception_ptr
  make_exception_ptr(_Ex) _GLIBCXX_USE_NOEXCEPT   // bits/exception_ptr.h:283，中间还夹着宏后缀
  { return exception_ptr(); }

struct S { T f(U) { return X(); } };              // 同一个形状在**类体**里：零诊断、9 个 ErrorNode（静默）
```

**现象**：两种表现，第二种是静默的：

```text
文件作用域   `a declarator takes only one initializer` —— 声明符被读成"变量 + 两个初始化式"
类体里       零诊断、9 个 ErrorNode —— 成员整块成瓦砾
```

闭包里 **5 个文件**的首错是第一种（`exception_ptr.h`、`predefined_ops.h`、`cmath`、`helper_functions.h`、
`type_traits.h`）。

**成因**：`T f(U)` 同时是两种读法——**一个取无名形参（类型 `U`）的函数**，或**一个用表达式 `U` 直接初始化的变量
`f`**——标准里两种都成立。这个 parser 的偏好是"初始化式读法优先"（它为 `Max(a, b);` 这种**没有类型**的声明而存在：
一串裸名字既是实参也是形参，而只有声明能有那个形状）。偏好没错，**缺的是证据**：声明符只有**一个**初始化式，
所以后面跟着 `{` 就说明那个括号组是形参表、`{` 是函数体。

**性质**：缺规则（判据缺一条证据），而且**证据要用真正的读取器去问**，不要另写一个扫描：

```text
试着把括号组读成形参表 → 让后缀读取器跑一轮（宏后缀 / noexcept / 尾随返回类型）→ 只有落在 `{` 上才保留
否则 rollback，继续走原来的偏好顺序
```

这样就不必把后缀词汇表抄第二遍（维护约定第 14 条：同一判据的第二处用法就是例外被漏掉的地方）。
**只在"函数定义合法"的地方问**——这一条本身有个坑：`is_inside_a_body()` 数的是"任何一个花括号引入的作用域"，
**类体也算**，所以第一版写成 `!is_inside_a_body()` 时 `struct S { T f(U) { … } };` 仍然读坏（它本来就是静默坏的）。
正确的判据是 `!is_inside_a_body() || is_at_class_member_level()`：命名空间体和 `extern "C"` 块本来就不算 body ✓，
**块**才算（那里 `T x(y) { }` 是"声明 + 块"，而且 C++ 里不能在函数里定义函数）。

**护栏**：`gaps.rs::a_body_settles_whether_the_group_was_a_parameter_list`——十四个读得出的拼写（文件作用域、
类体、命名空间、宏后缀、`noexcept`、尾随返回类型、两段真实文件里的整行）＋**形状**断言两侧：
`T f(U) { … }` 有 `ParameterList` 与 `CompoundStat` 而没有 `Initializer`、声明符名是 `f`；
`T x(y);` 反过来有 `Initializer` 而没有 `ParameterList`。反面还有块里那三条（`T x(y); { h(); }`、嵌套块、
lambda 体），它们是"**不**问这条证据"的地方。

**量到的**：128 个文件的闭包 372 → **366**、干净 92 → **93**；455 个文件的分析闭包 607 → **596**、
干净 371 → **375**（`exception_ptr.h`、`predefined_ops.h`、`cmath`、`helper_functions.h` 四个文件变干净，
`type_traits.h` 的首错从 165 行推到 231 行）。另外还有一个**数不出来的**修好：类体里的同名形状从
"零诊断 + 9 个 ErrorNode"变成正常成员。

### B63. 连着两个 C 风格转换：`(T)(U) x` —— 已修复

```c
return (__m512bh) __builtin_ia32_minmaxbf16512_mask ((__v32bf) __A,
                                                     (__v32bf)(__m512bh)      // avx10_2-512minmaxintrin.h:40
                                                     _mm512_setzero_si512 (),
                                                     (__mmask32) -1);
return ((PVOID) (LONG_PTR)InterlockedCompareExchange ((LONG volatile *) …));   // winbase.h
```

**现象**：`expected ), but get identifier` 打在第二个转换后面的那个名字上。4 个 `avx10_2*` 文件的首错都是它，
`winbase.h` 里同样的形状出现两次（1095 行与 3493 行）。

**成因**：`(T)…` 只在**有证据**时才读成转换——`(f)(x)` 是**调用**，不能把 callee 丢掉。这条证据是"`)` 后面
跟着一个操作数"（任何文法里两个操作数连排都不成句），而**`(` 被刻意排除**在那张表外（它是"延续"的开头）：
于是第一个 `)` 后面正是 `(`，问题根本没被问到，`(T)(U) 1` 读成"`(T)` 这个表达式调用 `(U)`"，`1` 无处可去。
**修法**：扫描时先**跨过一串配平的括号组**，再对最后一组后面的 token 问同一个问题。被跨过的恰恰是那个有歧义的
token，所以这个扫描是安全的：`(f)(a)`（后面什么都没有）与 `(f)(a)(b)`（最后一组后面是 `;`）保持**调用**读法。

**性质**：缺规则（判据缺一条证据，而证据的形状是"再往外一层"——与 B62 同族：都是"偏好没错、缺证据"）。
判据表 `<code>starts_an_operand</code>` 里那条"`(` 为什么不在表里"的注释因此多了一句：它不在表里，
但可以被**成串地跨过**。

**护栏**：`gaps.rs::a_cast_of_a_cast_is_still_a_cast`——十四个读得出的拼写（两层/三层转换、声明初始化式、
实参位置、两段真实文件里的整行、关键字类型）＋**形状**断言两侧：`(T)(U) 1` 是**两个** `CastExpr` 零个
`CallExpr`，`(f)(a)` 是一个 `CallExpr` 零个 `CastExpr`，`(f)(a)(b)` 是两个 `CallExpr`，
`(f)(a) + 1` 仍是调用（`+` 有歧义，保留调用读法是刻意的）。

**量到的**：455 个文件的分析闭包 干净 375 → **379**、报错 80 → **76**、消息 596 → **496**（4 个 `avx10_2*`
文件一起变干净，`winbase.h` 的首错从 1095 行推到 3493 行）；128 个文件的闭包不受影响（那些头不在它的闭包里）。

### B64. 条件写在**模板头里**、或写在**名字段**的位置 —— 已修复

```cpp
template<typename _Tp, bool _TreatAsBytes =           // bits/cpp_type_traits.h:620
#if __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
      __is_integer<_Tp>::__value
#else
      __is_byte<_Tp>::__value
#endif
        >
```

```cpp
    basic_string<_CharT, _Traits, _Alloc>::           // bits/cow_string.h:3900（vector.tcc:133 同形）
#ifdef __glibcxx_string_resize_and_overwrite
    resize_and_overwrite(const size_type __n, _Operation __op)
#else
    __resize_and_overwrite(const size_type __n, _Operation __op)
#endif
```

**现象**：三处各自的首错——`expected primary expression`（模板实参）、`expected a name`（`::` 之后）、
`expected a template argument`。**4 个文件**因此失败：`cpp_type_traits.h`、`cow_string.h`、`vector.tcc`、
`stl_iterator.h`（第四个见下）。

**成因**：指令接缝这一族（§2.1）此前的十处都在**语句/声明/成员/初始化式/要求**上，模板头里那几处没有：

```text
模板实参表里（`X<` 与参数之间、以及参数与参数之间）    parse_template_argument_list_inner
模板形参表里（参数之间、以及 `=` 与默认值之间）        parse_template_parameter_list / parse_template_parameter
名字段的位置（`::` 之后，或者文件作用域的第一个段）    parse_name
```

**性质**：缺规则（同一族的最后三处），但**不是照抄一句 `while Hash` 就完**——两处需要"分支意识"：

1. **默认值按分支各写一遍**：`= #if X 1 #else 2 #endif >` 里 `#else` 不是"下一个形参"，而是**同一个形参**的
   另一种拼法，所以读完一个值之后要看下一条指令是不是 `else`/`elif`，是就再读一个值（值读取被抽成
   `parse_a_default_value`，两处调用，不抄第二遍）。
2. **`#endif` 结束的是形参，不是列表**：读完值之后游标在 `#endif` 上，列表要问的是 `,`/`>`——第一版写成
   "读指令后 `continue`"（那是在要求**另一个形参**），于是 `>` 上又报一次 `expected a type specifier`。
   正确做法是先把指令读完、再拿终止符去 match。

**护栏**：`gaps.rs::a_conditional_may_decide_a_template_head`——十一个读得出的拼写（单分支/双分支的默认值、
`typename` 默认值、参数之间、实参、名字段、两段真实文件的整行）＋**形状**断言（三条指令属于**实参表**自己，
且**每个分支各贡献一个实参**——条件写了两遍，实参就有两个）＋一条 `assert_does_not_read_yet` 钉住下面那个形状。

**量到的**：128 个文件的闭包 干净 93 → **95**、消息 366 → **358**；455 个文件的分析闭包 干净 379 → **382**、
报错 76 → **73**、消息 496 → **487**。`cow_string.h`、`cpp_type_traits.h`、`vector.tcc` 三个文件变干净。

### B65. 声明按分支各写一遍，而每个分支自带**尾巴和分号** —— 待修

```cpp
template<typename _InputIterator>
    using __iter_key_t = remove_const_t<               // bits/stl_iterator.h:3090
#ifdef __glibcxx_tuple_like // C++ >= 23
      tuple_element_t<0, typename iterator_traits<_InputIterator>::value_type>>;   // ← 关掉 `remove_const_t<` 并结束声明
#else
      typename iterator_traits<_InputIterator>::value_type::first_type>;           // ← 同样关掉并结束
#endif
```

**现象**：`expected (, but get >` 打在**第二个分支**的尾巴上——第一个分支已经把声明读完了（`>>;`），
第二个分支的文本落在一个"已经结束的声明"后面。

**成因**：共享的部分是 `remove_const_t<`（**在 `#if` 之前**），每个分支各写"实参 + 闭合的 `>`/`>>` + `;`"。
所以这不是 B64 那三处接缝的形状——那三处是"一个位置上的 token 被条件替换"，这里是**一条声明的尾巴被条件
替换了两遍**。与第十一轮的 `parse_a_definition_per_branch` 同族，但那一条要求每轮重新出现 `=`，而这里
`=` 是共享的。

**性质**：缺规则（"按分支写一遍"的第三种用法）。做法：让别名/声明读取器接受"每条分支一个尾巴"，即
在 `;` 之后若游标在 `#else`/`#elif` 上，就读指令、再读一条尾巴与它的 `;`；`#endif` 收尾。
**先量再改**：它现在只值 1 个文件（`stl_iterator.h`），而改动落在"声明何时结束"这条最敏感的判据上——
所以先把它钉住（`gaps.rs` 的 `assert_does_not_read_yet`），等队列里它升到前面再动。

**护栏**：`gaps.rs::a_conditional_may_decide_a_template_head` 末尾那条 `assert_does_not_read_yet`。

### B66. 推导指引（deduction guide）：`M(I) -> M<I>;` —— 已修复

```cpp
  template<typename _InputIterator, typename _Allocator,
	   typename = _RequireInputIter<_InputIterator>,
	   typename = _RequireAllocator<_Allocator>>
    multimap(_InputIterator, _InputIterator, _Allocator)          // bits/stl_multimap.h:1153
    -> multimap<__iter_key_t<_InputIterator>, __iter_val_t<_InputIterator>,
		less<__iter_key_t<_InputIterator>>, _Allocator>;
```

**现象**：`expected ;` 打在 `->` 那一行的**续行**上（`less<…>` / `M<I>`）。

**成因**：C++17 的推导指引没有返回类型，写成"模板头 + 看着像调用的东西 + `-> 类型`"。这个 parser 的初始化式
偏好（为 `Widget w(T)`、以及没有类型的 `Max(a, b);` 而设）把那个括号组读成**直接初始化**，于是 `->` 落在
"初始化式之后"——而初始化式后面不能有 `->`，声明到此为止。

**性质**：缺规则，而且证据是 B62 那条证据**旁边的一个 token**：`-> T` **只属于函数声明符**（变量的初始化式
后面不可能有它）。所以修法是在 B62 那段试读里再加一条保留条件——而且必须**在限定符读取器之前问**：

```rust
let a_trailing_return_type_follows = p.current_token() == CppTokenKind::Arrow;  // ← 先问
super::types::eat_function_qualifiers(p);                                       // ← 它会把 `-> T` 吃掉
if a_trailing_return_type_follows || p.current_token() == CppTokenKind::LeftBrace { 保留函数读法 }
```

第一版把这一步写在 `eat_function_qualifiers` **之后**，于是什么都没看见——尾随返回类型已经被包进
`TrailingReturnType` 节点了。

**护栏**：`gaps.rs::a_deduction_guide_is_a_declarator_with_a_trailing_return_type`——十个读得出的拼写
（四种指引形状含真实的 `multimap` 与 `basic_string_view`、普通尾随返回类型、成员函数、lambda 的 `->`）＋
**形状**断言（指引是"一个形参表 + 一个 `TrailingReturnType` + 零个 `Initializer`"）。反面在这里格外重要：
`->` 同时是**成员访问**运算符，所以 `(a)->b` 必须仍然是初始化式里的表达式，而不是被读成声明符
（断言：那个 `auto r = …` 有一个 `Initializer`、零个 `TrailingReturnType`）。

**量到的**：128 个文件的闭包 干净 95 → **97**、消息 358 → **353**；455 个文件的分析闭包 干净 382 → **386**、
报错 73 → **69**、消息 487 → **480**。四个文件变干净：`bits/map`、`bits/multimap`、`bits/stl_multimap.h`、
`string_view`。

### B67. `asm` 语句（payload 不是 C++）—— 已修复

```cpp
  __asm__ volatile ("tilerelease" ::);                    // amxtileintrin.h:56
  __asm__ __volatile__("int {$}3":);                      // _mingw.h:584
  __asm__ __volatile__ ("pconfig\n\t" : "=a" (retval) : "a" (leaf) : "cc");
```

**现象**：`expected ; after expression` 打在 `__asm__ volatile (…` 这一行上——`asm` 被读成一个普通名字，
于是整条语句按"表达式语句缺分号"处理。2 个文件的首错是它（`amxtileintrin.h`、`_mingw.h`），
而语料里 **69 处** `asm` 拼写分布在 8 个文件里（多数在 `#define` 体内，所以只值 2 个文件的首错）。

**成因**：这一族根本没有规则——parser 里没有任何 `asm` 处理（grep 零命中）。而且**payload 不是 C++**：
`"int {$}3":`、`[ret] "=r" (ret)`、`"a" (leaf)` 是编译器的操作数语言，任何表达式/形参规则都读不了；
第二、第三个操作数段还经常是**空的**（`::` 与 `:`），所以它连"逗号分隔的列表"都不是。

**性质**：缺规则。做法与理由都值得记：

```text
形状    asm | __asm | __asm__            ← 三个拼写都是编译器自己的（C 的关键字 + GCC/MSVC 的扩展）
        + 可选 volatile / inline / goto（GCC 的 __volatile__ 在词法上是普通名字，一并接受）
        + 一个**配平的组**：GCC 是 ( … )，MSVC 是 { … }
读法    payload **一个 token 一个 token** 收进 AsmStat 节点——它是什么就留什么
        分号：`( … )` 之后要有；`{ … }`（MSVC 的块）之后不要
```

**一个 token 一个 token 地留**是这一条的要点：给 payload 编一套文法等于给一门这一层不实现的语语言编文法，
而且会把用户想看的文本弄丢（高亮、hover、成块搬动 asm，要的都是**原文**）。这与宏调用的实参用
`ArgumentList` 装"原始配平 token"是同一个安排。

**新节点 `CppSyntaxKind::AsmStat`** 有两个细节：① 它**加在枚举最末**——`kind/mod.rs` 的原始值转换是
`transmute`、上界就是"最后一个 variant"，加在中间会把已存下来的 kind 判别值改掉；② 那个上界与断言它的
单元测试（`out_of_range_raw_is_rejected`）要跟着改，测试里现在写的是 `AsmStat`，并且注释说明了**新增 kind
要加在它之后**。

**证据优先于拼写**：`asm` 不是 C++ 关键字（所以词法器给的是普通标识符），三个拼写是"编译器自己的"这条依据
撑起这条形状判据；但**本文件的 `#define`（或调用方的表）优先**——文件自己 `#define asm(x)` 时走宏的规则，
测试里钉了这一条。

**护栏**：`gaps.rs::an_asm_statement_keeps_its_payload_as_tokens`——九种读得出的拼写（两个真实文件的整行、
四个操作数段的完整形态、空的 `::` 与 `:`、`asm goto`、折行版、MSVC 的 `__asm { … }`）＋**形状**断言
（节点文本从 `__asm__` 到 `);`、每个操作数都还在文本里、没有 `ErrorNode`）＋宏证据的反面。

**量到的**：128 个文件的闭包 消息 353 → **347**；455 个文件的分析闭包 干净 386 → **388**、报错 69 → **67**、
消息 480 → **464**（`amxtileintrin.h` 与 `_mingw.h` 两个文件变干净）。

### B68. 转换读法是**偏好**：组里其实是函数式转换时，它要退回去 —— 已修复

```cpp
      __gam1 = (__gammi - __gampl) / (_Tp(2) * __mu);                 // tr1/bessel_function.tcc:114
      __fact *= __k / (_Tp(2) * __numeric_constants<_Tp>::__pi());    // tr1/gamma.tcc:117
      static const _CASable _CASable_mask = ((_CASable(1) << (_CASable_bits / 2)) - 1);
      _Tp __p_lm = (_Tp(2 * __j - 1) * __x * __P_lm1m …);             // tr1/legendre_function.tcc:175
```

**现象**：`expected ), but get (`（或 `get *`）打在那一行的中段，整条声明成瓦砾。**12 个文件**的首错是它：
九个 `.tcc` 数学实现加 `parallel/types.h`、`bits/stl_bvector.h`、`bits/max_size_type.h`、`bits/type_traits.h`。

**成因**：`(T)…` 是靠**证据**读成 C 风格转换的——括号里那个名字是**本文件知道的类型**。而这条证据对
"**组里的表达式**用了函数式转换"同样成立：`_Tp` 是模板参数（模板头把它记成了类型名），于是 `(_Tp(2) * __mu)`
被当成转换 `(_Tp` + 期待 `)`，可下一个 token 是 `(`——那是 `_Tp(2)` 这个**调用**的开头。
真正的问题是**失败的尝试没有回退**：守卫说"转换"，那条 arm 就一路走到报错，括号表达式那条读法根本没轮到。

**性质**：缺规则（缺的是"偏好失败之后怎么办"这一步），而**守卫自己的注释早就写好了答案**：
"a cast whose operand fails to parse is rewound and read as a parenthesised expression"——缺的只是**类型那一半**
失败时也回退。一个 checkpoint 就够：失败的 `parse_type_id` 与它报的错一起消失（`rollback` 会截断诊断，
"没人采纳的读法报的问题不是这个文件的问题"），表达式规则拿到本该属于它的 token。

这一条因此是**判据的收尾**而不是新判据：守卫给的"可能是转换"本来就只是偏好，现在失败时会真的退回去。

**护栏**：`gaps.rs::a_group_holding_a_call_is_an_expression_not_a_cast`——十四个读得出的拼写（四种真实形状、
`(T(2 * j - 1) * x * y)`、`((T(1) << (n / 2)) - 1)`、带模板头的两种）＋**形状**断言两侧：
`(T)x` 与 `(T(*)(int))x` 仍然是 `CastExpr`；`(T(2) * c)` 是 `ParenExpr(BinaryExpr(CallExpr(T, 2), *, c))`——
**没有** `CastExpr`；`(T(2))` 同样是调用而不是"把 2 转成 T"＋一条**诊断**断言（被放弃的转换尝试报的
`expected ), but get (` 必须被回退收回去）。

**量到的**：128 个文件的闭包 干净 97 → **100**、报错 31 → **28**、消息 347 → **283**；
455 个文件的分析闭包 干净 388 → **400**、报错 67 → **55**、消息 464 → **352**。12 个文件变干净，
另有三个文件（`compare`、`intrin-impl.h`、`winbase.h`）的首错往后移。

### B69. 形参表里的一条接缝：指令在形参之间，或把形参按分支写两遍 —— 已修复

```cpp
      _M_insert_(_Base_ptr __x, _Base_ptr __p,                       // bits/stl_tree.h:2468
#if __cplusplus >= 201103L
		 _Arg&& __v,
#else
		 const _Val& __v,
#endif
		 _NodeGen& __node_gen)
      {

    random_shuffle(_RAIter, _RAIter,                                 // parallel/algorithmfwd.h:700
#if __cplusplus >= 201103L
		   _RandomNumberGenerator&&);
#else
		   _RandomNumberGenerator&);
#endif
```

**现象**：`expected ), but get identifier` / `expected primary expression` 打在指令后面那个形参那一行，
整条声明成瓦砾（`stl_tree.h` 是 `2468:27`，`algorithmfwd.h` 是 `704:28`）。模板形参表和 requires 表达式
**体内**早就各有接缝（B57/B64），**形参表**没有。

**成因**：`parse_parameter_list` 的循环第一次遇到 `#` 就当作列表结束，于是 `(` 之后紧跟的指令被丢在列表外，
列表只剩前半截。而这条接缝上一个 `#` 有三种写法，处理各不相同：

```cpp
void f(int a, #if X int b, #endif int c);        // ① 指令**在形参之间**：读掉，继续读形参
void f(int a, #if X int b #else long b #endif ); // ② 形参**按分支写两遍**：`#else`/`#elif` 说的是
                                                 //    "**这个**形参换个拼法"，所以接着再读**一个**形参
void f(int a, int b, #if X int c); #else int c); #endif
                                                 // ③ 其它指令只"关闭"什么，接着是终结符（`)`）
```

②与③靠指令的**名字**分开，不是靠"这里有没有 `#`"——这是量出来的：`algorithmfwd.h:703` 的 `#else`
恰恰属于③（它结束的是**第一支整条声明**），把它当②会多读一个形参。

**性质**：缺规则（接缝位置），与维护约定"接缝要知道自己坐在谁里面"同族。三种写法一次落地，因为它们是
同一条接缝的三个面：读完指令**之后**要回答的问题只有一个——"这里是形参，还是终结符"。

**护栏**：`gaps.rs::a_directive_may_decide_a_parameter_or_stand_between_two`——五种读得出的拼写（指令在
两个形参之间、在第一个形参之前、`#elif` 三分支、带函数体的定义）＋**形状**断言（整条列表**只有一个**
`ParameterList`，形参一个不少）＋一条如实的断言：②的两种拼法出**两个** `Parameter` 节点（parser 没有
"这两个分支互斥"的表，把两个拼法都留下是诚实的树；错的是一条列表变成两条、或者 `long b` 消失）。
B65 那条（第二支是**片段**）用 `assert_does_not_read_yet` 钉住，并写明文件与行号。

**量到的**：128 个文件的闭包 干净 100 → **101**、报错 28 → **27**、消息 283 → **280**；455 个文件的分析闭包
干净 400 → **401**、报错 55 → **54**、消息 352 → **348**。`bits/stl_tree.h`（2468 那条首错）整个文件变干净；
`parallel/algorithmfwd.h` 的首错 701 → **704**，也就是被推到这条接缝管不着的那个**片段**上——那正是 B65。

### B70. 宏的实参不是表达式：调用读不出来时，实参组按 token 留下 —— 已修复

```cpp
_MM_REDUCE_OPERATOR_BASIC_EPI16 (+);                                    // avx512vlbwintrin.h:4992
if constexpr (__is_same(const volatile _Tp, const volatile void))       // bits/new:234
return __reference_constructs_from_temporary(_Elements, _Up&&);         // tuple:922
_GLIBCXX_TYPEID(typename std::iterator_traits<_Iterator>::value_type);  // bits/formatter.h:485
if (TlsSetValue (__key, CONST_CAST2(void *, const void *, __ptr)))      // gthr-default.h:723
auto n = __glibcxx_min(char);                                           // limits:465
SHSTDAPI_(WINBOOL) InitNetworkAddressControl (void);                    // shellapi.h:883
```

**现象**：`expected primary expression` 打在实参上（`+`、`const`、`void`、`char`、`typename`），整条语句成瓦砾。
**9 个文件**的首错是它：`avx512vlbwintrin.h`、`bits/stl_pair.h`、`bits/formatter.h`、`limits`、`new`、
`gthr-default.h`、`emmintrin.h`、`xmmintrin.h`、`shellapi.h`。

**成因**：宏的实参是**宏自己的 token**，这条读法 parser 早就有（`parse_balanced_token_group`，语句形式与
"宏站在声明位置"两条规则都在用，见 B32/B36 那一族）——缺的是**调用**这条 arm：它只认实参是表达式或
花括号初始化式。文件 `#define` 过的函数式宏走不了"语句宏"那条（它的体不是完整语句），于是 `M (+);`
被当普通调用、实参 `(+)` 读不出。而语料里这类宏**绝大多数来自被包含的头文件**——`__is_same`、
`_GLIBCXX_TYPEID`、`CONST_CAST2` 的 `#define` 都不在这个文件里，parser 手里的表没有它们。所以判据不能问
"这个名字是不是宏"（问不到），只能问"这次调用的实参读得出来吗"。

**性质**：缺规则，而且是**判据的收尾**（与 B62/B66/B68 同类，见"一个反复出现的教训"）：偏好（实参是表达式）
先试，失败才换成 token 组，且失败那次的诊断随 `rollback` 一起消失。**代价是真的，且写在护栏里**：
`g(1 +)` 这样真正坏掉的调用现在**不报错**了——没有任何形状能分开这两者，因为宏的实参**就是**任意
token（`_MM_REDUCE_OPERATOR_BASIC_EPI16 (+)` 与 `g(1 +)` 的 token 序列本身没有区别）。选的是"一个坏调用的
诊断"与"每个头文件里每个宏的每次使用"之间的取舍：**取舍不是缺口**，所以它是 B70 而不是待修条目。

**护栏**：`gaps.rs::a_macros_arguments_that_are_not_expressions_stay_tokens`——七种读得出的拼写，全写在函数体里
（含 `__attribute__` 转换做实参、`typename` 做实参、`if constexpr` 里、`return` 里），外加文件作用域那条
邻接形状（`SHSTDAPI_(WINBOOL) f (void);`）＋**形状**断言两侧：读不出来的那组是 `CallExpr > ArgumentList`
（原样的 token），而 `g(1, 2)`、`g(h(x), {1, 2})` **没有** `ArgumentList` 节点＋那条**代价**本身
（`g(1 +)` 有 `ArgumentList`、无诊断）。钉住代价是为了将来收窄这条规则时，第一个失败在写清理由的地方。
邻接形状的边界也钉着：`SHSTDAPI_(WINBOOL) f (void);` 读得出（那是**声明**读法），
`WINOLEAPI_(void) CoUninitialize (void);` 读出**调用**、后面的声明符仍读不出（Mingw 五个头文件那一族，
还在队列上）。

**量到的**：128 个文件的闭包 干净 101 → **103**、报错 27 → **25**、消息 280 → **260**；455 个文件的分析闭包
干净 401 → **410**、报错 54 → **45**、消息 348 → **278**。9 个文件变干净，**两份清单都没有一个文件从干净
变报错**。一处如实记下：`bits/stl_algobase.h` 的首错往前挪了一行（161 → 160），是**同一个缺陷**（宏调用
当语句、没写分号）报在了参数组收尾处，而不是下一行那个标识符上。

### B71. `using enum E;` 与 `register`：两条**根本没有规则**的拼写 —— 已修复

```cpp
	  using enum _Fp_fmt;                          // compare:710（C++20 using-enum-declaration）
register unsigned int r0 __asm__("r0") = code;     // _mingw.h:607（C 头文件遗留的存储类 + GNU asm 标签）
```

**现象**：`using enum _Fp_fmt;` 报 `expected a name`（`using` 规则把关键字 `enum` 当成了要引入的那个名字）；
`register` 开头的声明报 `expected primary expression` 落在 `register` 上——它**不在**"声明可以以什么开始"
那张表里，于是语句层根本没问过声明那条读法，直接把 `register` 当表达式读。

**成因**：两条都不是"哪种读法"的问题，是**缺规则**：C++20 的 using-enum-declaration 从来没写；
`register` 在 lexer 里是一个 token kind（`RegisterKeyword`），但在 grammar 里没有任何一处接受它——既不在
`can_begin_a_declaration`，也不在存储类说明符表里。

**性质**：缺规则，两条都**没有第二种读法**可争：`using` 后面的名字不可能是关键字 `enum`；`register` 不能
开始一个表达式。所以两条都不需要偏好或回退，各加一处即可。

**做法**：`using enum` 在 `parse_using_declaration` 里加一条——读掉 `enum`、读**一个**名字（可带限定，
和 `using ns::f;` 同形）、要分号；**不记录**这个名字是类型：这条声明引入的是枚举量，不是类型名，而本
parser 只记"一个名字是什么"（与别名形式的注释同一条理由）。`register` 进 `storage_or_function_specifier`
（新节点 `RegisterSpec`，与 `StaticSpec`/`MutableSpec` 同一张表、同一个位置）并进 `can_begin_a_declaration`。
新 kind 照 B67 的规矩**加在枚举最末**（原始值转换是 `transmute`、上界是最后一个 variant），
`kind::tests::out_of_range_raw_is_rejected` 的名字跟着改。

**顺带读出来的第三件事（如实钉住，不算功劳）**：`r0 __asm__("r0")` 这种 **GNU asm 标签**此前也没有规则，
`register` 一放行它就跟着通了——它被读成 `InitDeclarator > Declarator(r0) + MacroCall(__asm__("r0"))`。
`asm` 的 payload 不是 C++（B67 同一条理由），所以"原样留 token"是能接受的读法，但它**不是**声明"这就是
宏调用"；护栏里把形状钉住，将来给它一个自己的节点时，第一个失败会落在写清理由的地方。

**护栏**：`gaps.rs::a_using_enum_declaration_and_the_register_specifier_read`——`using enum` 在文件/函数体/
类体三处、带限定名；`register` 在函数体（含 `for` 头里）、文件作用域；**形状**断言：`register` 出
`RegisterSpec` 且 `using enum` 出 `UsingDecl` 且没有 `MissingNode`；asm 标签那条如实断言"无诊断 + token 在
`MacroCall` 里"。

**量到的**：128 个文件的闭包 干净 103 → **105**、报错 25 → **23**、消息 260 → **255**；455 个文件的分析闭包
干净 410 → **412**、报错 45 → **43**、消息 278 → **273**。两个文件变干净（`compare`、`_mingw.h`），
两份清单都**没有一个文件从干净变报错**。

### B72. "宏站在声明的头部"这一族：七种写法、四种成因 —— 部分已修（三条已修，两条待修）

上一批（B70）之后，首错清单里**最大的一族**是 Mingw 的 CRT/COM 头，一共 **8 个文件**。逐条量过（每条都在
`fixtures` 里最小复现过），**它们不是一个构造**——这正是这一族难的地方：

```text
文件:行                          写法                                            状态
basetsd.h:11 / corecrt.h:35     __MINGW_EXTENSION typedef unsigned __int64 …   **已修**（`__int64` 是宏写法的名字）
oleauto.h:71                    WINOLEAUTAPI SafeArrayAccessData(SAFEARRAY *psa, void HUGEP **ppvData);
                                                                               **已修**（两层：前缀宏 + 形参里的宏）
rpcnsi.h:25                     RPCNSAPI RPC_STATUS RPC_ENTRY RpcNsBindingExportA(…)
                                                                               **已修**（宏 + 类型 + 宏 + 声明符）
objbase.h:96 / ole2.h:58        WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);   **已修**（B73）
commdlg.h:577                   STDMETHOD(QueryInterface) (THIS_ REFIID riid,…) PURE;  待修
winperf.h:180                   typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);        待修
```

**已修的三条有一个共同的机制**：一个**写成宏样子的名字**可以站在**类型与声明符之间**（或紧跟类型之后），
成为类型的又一个词——

```cpp
unsigned __int64 POINTER_64_INT;   // 类型 `unsigned __int64`，声明符 `POINTER_64_INT`
void HUGEP **ppvData               // 类型 `void HUGEP`，声明符 `**ppvData`
RPCNSAPI RPC_STATUS RPC_ENTRY f(); // 三个名字里两个是宏，`RPC_STATUS` 才是类型
```

**成因**：`name_joins_the_type` 原本只在**已经写进类型的那个词是"名字"**（`MY_API Widget *p` 这一形）时才允许
第二个名字加入；类型是**内建关键字**时（`void`、`unsigned`）一律不许——于是 `unsigned __int64 x;` 被读成
"类型 `unsigned` + 声明符 `__int64` + `x` 是一个**替声明站位的 MacroCall**"。那是 **A0 类静默错树**：没有诊断、
没有 `ErrorNode`、token 一个不少，构造却是错的。MinGW 头文件里"`unsigned __int64` + 变量"到处都是，
所以这不是角落。

**性质**：这条是**拼写约定**在起作用，必须说清为什么这次可以用：`Type Name Name` 这三个 token 有**两种相反的
真实读法**，而**形状本身分不开它们**——

```cpp
unsigned __int64 x;      // 加入类型的那个名字是**类型**：`__int64` 写成宏的样子
int x MY_DECL_SUFFIX;    // 加入类型的那个名字是**声明符**：`x` 不写成宏的样子，宏是**后缀**
```

第一版把条件放宽成"已经命名过类型就算"，于是第二行被读成"类型 `int x` + 声明符 `MY_DECL_SUFFIX`"——变量名
丢了，等于用一个 A0 错树换另一个。**两个已有测试当场抓住**：`a_macro_can_stand_among_a_declarators_suffixes`
（宏后缀那一形）与 B71 的 asm 标签那条断言。所以判据收窄成"**加入的那个名字要写成宏的样子**"
（`types::written_like_a_macro`：下划线开头，或 `decls::looks_like_a_macro_name` 的全大写），而它与
B32/B36 里被否掉的用法不同：那里是问"这东西**是不是**宏"（没有证据，只能猜），这里是**在两种都真实的读法
之间选一个**，且只在有类型、且有声明符跟在后面时才问（`a_declarator_still_follows_the_name`）。

**护栏**：`gaps.rs::a_macro_may_stand_between_the_type_and_the_declarator`——十二种文件作用域的写法（含
`typedef unsigned __int64 POINTER_64_INT;`、`void f(void HUGEP **ppvData);`、真实的 `WINOLEAUTAPI …` 行）
＋两种函数体里的写法＋**形状**断言三条，正反都钉：`unsigned __int64 x;` 是"说明符两个词、没有 `MacroCall`、
声明符是 `x`"；`int x MY_DECL_SUFFIX;` 正好相反（说明符一个词、有 `MacroCall`、声明符仍是 `x`）；
`MY_API Widget *p;` 保持"宏在类型里"。

**量到的**：128 个文件的闭包 干净 105 → **106**、报错 23 → **22**、消息 255 → **249**；455 个文件的分析闭包
干净 412 → **417**、报错 43 → **38**、消息 273 → **212**。**5 个文件变干净**（`avx512fintrin.h`、`_bsd_types.h`、
`corecrt.h`、`oleauto.h`、`rpcnsi.h`），**两份清单都没有一个文件从干净变报错**。
放宽的第一版在同一份语料上量到同样的数字（417/38/212）——也就是说**收窄没有花掉任何收益**，
而它保住了那两条读法。

**还剩下的两条**（下一轮）：
1. ~~`typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);`~~ —— **已修**（B74：宏在括号声明符里、名字之前）。
2. `STDMETHOD(QueryInterface) (THIS_ REFIID riid, LPVOID *ppvObj) PURE;`（`commdlg.h:577`）——宏站在声明头部
   （B73 已经会读这一半），但它的声明符**没有名字**：名字在宏自己的实参里
   （`#define STDMETHOD(method) virtual HRESULT STDMETHODCALLTYPE method`）。读成声明就会声明一个没有名字的函数，
   所以这条要么等**按位置**的宏证据，要么明说成"名字不可知"，不能靠形状。
3. `basetsd.h` 的首错已经推到 68 行（`static __inline unsigned __LONG32 HandleToULong (const void *h)` 的函数体里），
   是另一条形状，下一轮重新量。
另外 `combaseapi.h` 的首错已经推到 327 行（`COWAIT_DISPATCH_CALLS = 8,` 那种枚举值），与这一族无关了。
这**不写进** `assert_does_not_read_yet`：三条的形状各不相同，钉成"永远读不出"会挡住第 1、3 条。

### B73. 宏调用站在**声明头部**（`WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);`）—— 已修复

```cpp
WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);      // objbase.h:96
WINOLEAPI_ (void) OleUninitialize (void);              // ole2.h:58
```

（`combaseapi.h:35` 把 `WINOLEAPI_(type)` 展开成 `EXTERN_C DECLSPEC_IMPORT type STDAPICALLTYPE`——也就是说这个宏
**就是**整条声明的类型部分，而它的 `#define` 在**另一个文件**里。）

**现象**：`expected ; after expression` 打在那条声明的**声明符**上（`CoFreeLibrary`，列 17）。B70 之后
`WINOLEAPI_(void)` 被读成一个**调用**，错误只是从"实参读不出"变成"调用后面还有声明符"。

**成因**：三种读法都不对——说明符序列把 `WINOLEAPI_` 当类型、把 `(void)` 当它的形参表，声明出来的是一个叫
`WINOLEAPI_` 的函数，`CoFreeLibrary` 无处可去；表达式读法（B70 的回退）把调用读成**语句**；而"宏替声明站位"
那条只认"名字后面什么都没有，或只有 `}` `#`"。

**性质**：缺规则（"宏带实参表当说明符"这一形），判据是**形状**而不是证据——名字**带一个配平组**、组后紧跟
**一个标识符**（`MACRO(args) name (…)`）。三条边界，每条都让已有的主人继续拥有自己的形状：
- 名字是本文件 `#define` 过的 → 它有**体**，交给知道体的规则（证据优先，`MacroNames` 的老次序）；
- 组后面是 `;` 或 `{` → `FOO(x);`（语句 / 替声明站位的宏）与 `FOO(x) { }`（定义）各有主人；
- 名字是**编译器自己的拼写**（`__attribute__` / `__declspec`）→ 它有读者。**这一条是被量出来的**：第一版没有
  它，于是 `__attribute__((__nonnull__)) void f(…)` 也满足"名字 + 配平组 + 标识符"，被读成"宏说明符"，说明符
  序列在属性处就结束，跟在后面的 `_Rb_tree_node_base* _Rb_tree_rebalance_for_erase(…)` 没有类型——
  **`bits/stl_tree.h` 从干净变成报错**。同一次普查的三个数字（干净 +3、消息 −30）本来已经"赢了"，
  把逐文件清单对一遍才看见这一条；这就是"**按文件数报进度**"那条约定存在的理由。

**读数的方式**：宏调用产生的是 `MacroCall`（名字 + `ArgumentList`，token 原样），放在 `DeclSpecifierSeq` 里，
然后**说明符序列到此为止**——宏展开成什么是这一层不知道的，所以它后面读到的名字可能是声明符、也可能是类型的
又一个词；把它留给拥有声明符的读取器，`CoFreeLibrary (HINSTANCE hInst)` 就是一条普通的函数声明符。

**护栏**：`gaps.rs::a_macro_may_stand_between_the_type_and_the_declarator`（与 B72 前半同一个测试）——正面四种
写法之外，把**那条回归的两个真实声明**（`__attribute__((__nonnull__)) void f(const bool __insert_left);` 和
`_Rb_tree_rebalance_for_erase` 那条三行的）钉进"读得出"清单，再加一条"**声明符是谁**"的形状断言：
`WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);` 的 `InitDeclarator` 必须是 `CoFreeLibrary (HINSTANCE hInst)`
——反过来的读法会声明一个叫 `WINOLEAPI_` 的函数。边界也写着：`STDMETHOD(QueryInterface) (…) PURE;` **不在**这里，
它的声明符**没有名字**（名字在宏自己的实参里，见 B72）。

**量到的**：128 个文件的闭包 干净 106 → **108**、报错 22 → **20**、消息 249 → **124**；455 个文件的分析闭包
干净 417 → **420**、报错 38 → **35**、消息 212 → **181**。`objbase.h`、`ole2.h`、`stl_map.h` 变干净，
`stl_heap.h` 在 128 那份清单里也变干净；**两份清单都没有一个文件从干净变报错**（含修掉的那次回归）。



### B74. 同一个宏写法的名字，再往下两层：**type-id 里**与**括号声明符里** —— 已修复

```cpp
static __inline unsigned __LONG32 HandleToULong (const void *h)              // basetsd.h:68
{ return ((unsigned __LONG32) (ULONG_PTR) h); }                              // 转换的 **type-id**
typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);                                 // winperf.h:180
typedef DWORD (WINAPI PM_COLLECT_PROC)(LPWSTR,LPVOID *,LPDWORD,LPDWORD);     // winperf.h:181
```

**现象**：两个位置各报各的——转换 `(unsigned __LONG32)` 报 `expected primary expression`（`basetsd.h` 全局 7 条错，
全是这一形），`typedef` 那四条报 `expected ;` 打在第 15 列的 `WINAPI` 上。

**成因**：B72/B73 让"写成宏样子的名字"站在**类型与声明符之间**，但同一拼写还有两个位置：
- **type-id 没有声明符**，于是调用方给的 `allow_second_name` 是 `false`——这对它本来要回答的问题是对的
  （type-id 里多一个*名字*会把 `template <typename T, typename U>` 连成一个），但对"这个名字写成宏的样子"
  就错了：`(unsigned __LONG32)` 是一个转换，拒了它整条转换就读不出来；
- **括号声明符里名字之前**：`(WINAPI PM_OPEN_PROC)` 里 `WINAPI` 是 `__stdcall`，而守卫
  `a_parenthesised_declarator_with_a_name_follows` 只认 `(*f)`、`(&f)`、`(C::*f)` 三形，于是这一组被
  `parse_abstract_declarator` 当**形参表**读——`WINAPI` 成了一个参数的类型，`PM_OPEN_PROC` 成了参数名。

**性质**：缺规则，两处都用同一个判据（[`written_like_a_macro`]）收口：type-id 里"类型已经命名过 + 这个名字写成
宏的样子 ⇒ 它是类型的又一个词"（type-id 没有声明符，所以没有"后面还有没有声明符"可问）；声明符里
"括号内两个标识符连写 ⇒ 前一个是宏、后一个是这个名字"（`NAME NAME` 在任何文法里都不是一个声明符）。

**第一版把 `debug/safe_sequence.tcc` 弄脏了**——同一个族的第二例回归，而它比 B73 那次更隐蔽：
`void C::f(_Predicate __pred) { }` 的组与 `(WINAPI PM_OPEN_PROC)` **一模一样**，被当成括号声明符之后，
函数体里的声明落到了函数外面，错误浮现在**三行以下**的一个 `typedef` 上（`expected primary expression`）。
分开这两者的是**组的后面是什么**：形参表后面永远不会跟着另一个属于同一声明符的 `(`，而函数指针 typedef
永远跟着。加上这一条之后，`safe_sequence.tcc` 回来、`winperf.h` 的收益保住。

**顺带量到、但**没修**的一条**：`typedef DWORD (PM_OPEN_PROC)(LPWSTR);`（**不带宏**的同形）本来也读不出
（`expected ;` 打在第 29 列）——它的组里只有一个名字，本条的判据（两个标识符连写）不覆盖它。它不在语料的
首错清单上，所以只记在这里，不顺手放宽：那是"`(NAME)(params)` 是不是声明符"的另一个问题。

**护栏**：`gaps.rs::a_macro_shaped_name_inside_a_type_id_and_a_parenthesised_declarator`——type-id 三形
（`(unsigned __LONG32)`、转换链 `(void *) (LONG_PTR) (__LONG32) h`、`sizeof(unsigned __LONG32)`）＋
声明符四形（`PM_OPEN_PROC`/`PM_COLLECT_PROC`/`PM_CLOSE_PROC` 与真实的 basetsd 函数）＋**共形但必须保持原读法**
的五形（`void C::f(_Predicate __pred) { }`、`void C::g(_Predicate __pred);`、`void f(int (_Predicate __pred));`、
`typedef void (*fp)(int);`、`int x MY_DECL_SUFFIX;`）＋两条**形状**断言：形参表里 `__pred` 仍是参数名且没有
`MacroCall`（回归那一形），以及 `(WINAPI PM_OPEN_PROC)` 的 `MacroCall` 是 `WINAPI`、声明符文本带上了属于它的
`(LPWSTR)`（这正是"它是指向函数的指针"那句话）。

**量到的**：128 个文件的闭包不动（108 / 20 / 124——这一族全在 MinGW 头文件里，libstdc++ 闭包没有它们）；
455 个文件的分析闭包 干净 420 → **423**、报错 35 → **32**、消息 181 → **155**。`winperf.h`、`intrin-impl.h`、
`winbase.h` 变干净；**没有一个文件从干净变报错**（含修掉的那次回归）。`basetsd.h` 的错 7 → **3** 条，
首错推到 90 行（`return ((void *) (LONG_PTR) (__LONG32) (ULONG_PTR) h);` 那种成串转换里更靠后的一形）。

### B75. **匿名**类定义后面的那个名字：早退把它当成了类型 —— 已修复（一类 A0 静默错树）

```cpp
  union
  {
    __m128h __a[2];
    __m256h __v;
  } __u = { .__v = __A };          // avx512fp16vlintrin.h:155（avx512fp16intrin.h:2986 同形）
```

**现象**：带初始化式的那些报 `expected a declarator name`，打在 `=` 上（两个 `avx512fp16*` 头文件各一条首错）；
而**不带**初始化式的那些**什么也不报**——树里没有 `ErrorNode`、没有 `MissingNode`、token 一个不少，
只是那个变量**没有声明**。两种现象一个成因。

**成因**：`name_joins_the_type` 的第一条是"**还没有类型，所以这个名字只能是类型**"（`!has_type_specifier`）。
**有名字**的类定义（`struct S { … } x;`）会写一个名字，于是序列到声明符时 `has_type_specifier` 已经为真；
**匿名**定义（`union { … } u;`）一个名字都不写，那个标志仍是假——早退于是把 `u` 当成类型的又一个词，
声明出来的是"类型 `union { … } u`、一个声明符也没有"。类体**本身就是**一个完整的类型（这条判断早就在函数里，
只是排在早退**后面**，永远轮不到）：把它挪到早退**之前**就对了。

**性质**：判断顺序错（不是缺判据），属于 **A0 类静默错树**——它没有诊断，所以 `gaps.rs` 原来的三种护栏
（errors ∪ `ErrorNode` ∪ `MissingNode`）一条都拦不住；带初始化式的写法**之所以**会报错，只是因为
"初始化式需要一个名字"那条护栏（`an_initializer_needs_a_name`）顺手把它顶了出来。这也是为什么两种写法
必须**同时**钉住：只修报错的那一半，静默的那一半会留下来。

**护栏**：`gaps.rs::a_name_after_an_anonymous_class_definition_is_the_declarator`——函数体里六形
（有/无初始化式、`struct`/`union`、匿名成员类型、指针声明符）＋文件作用域五形（`typedef struct { … } Alias;`、
`enum E { A } e;`、`struct S { int a; } x;`、数组、`static union { … } value = { … };`）＋**形状**断言三条：
带初始化式时 `InitDeclarator` 是 `u = { 1 }`、不带时是 `u`（静默那一半），以及**含 `UnionDef` 的那个
`DeclSpecifierSeq` 的文本就是 `union { int a; }`**——`u` 不在类型里。

**量到的**：128 个文件的闭包 干净 108 → **109**、报错 20 → **19**、消息 124 → **119**；455 个文件的分析闭包
干净 423 → **426**、报错 32 → **29**、消息 155 → **147**。`avx512fp16intrin.h`、`avx512fp16vlintrin.h`、
`basic_string.tcc` 变干净（第三个是顺带：它的守卫对象写法同形），**两份清单都没有一个文件从干净变报错**。

### B76. 头文件里的宏**自己就是一条语句**（`__glibcxx_function_requires(…)`，没有分号）—— 已修复

```cpp
      __glibcxx_function_requires(_LessThanComparableConcept<_Tp>)     // bits/stl_algobase.h:237
      //return __b < __a ? __b : __a;
      if (__b < __a)
	return __b;
```

紧跟 `#endif` 的那一形（`bits/move.h:233`）与"连着两次调用"（`bits/stl_algobase.h:170`）同族。

**现象**：`expected ; after expression` 打在**下一条语句**上（`if`、`#endif`、下一个宏调用），
`move.h`、`stl_iterator_base_funcs.h`、`find.h` 的首错都是它。

**成因**：libstdc++ 把这些概念要求宏定义成**空的**，所以一次调用本身就是一条完整语句、没有分号；而它们的
`#define` 在**别的文件**里。**两种读法都真实**：`NAME ( 参数 )` 也是一条**函数声明**（名字是宏名、参数没有
名字），所以问题不是"读不出来"，而是"声明读法**成功**了，只是没有分号"——护栏
（`an_initializer_needs_a_name` 那一类）看不见这一形。

**性质**：缺规则 + **判据次序**，三条边界各由一次失败买来：
1. 名字必须是**实现保留的**（下划线开头）。`FOO(x)` 同样是宏的写法，但也是用户函数的写法，而用户自己的宏
   会在本文件 `#define`（那是证据，这条规则没有）。所以 `FOO(x)` 少了分号仍然是错误；
2. 组之后必须是一个**不能继续表达式**的 token（`if`、`#`、`}`、`return`、后面的名字、一条声明的开头）。
   `{` **不在**集合里 —— `g(x) { }` 是块形那条规则（B36）一直要权衡的错误；
3. 这条读法**排在声明读法之后**，而声明读法在这里会**成功**，所以还要一条"**它没有吃掉 `;`**"的判据把
   两者分开。**第一版把顺序反了**（抢在声明读法之前），于是把 B73 的 `WINOLEAPI_(void) CoFreeLibrary (…)`
   从"宏是声明的说明符"抢成了"宏语句 + 另一条声明"——`a_macro_may_stand_between_the_type_and_the_declarator`
   当场抓住。这一条与 B73 是**同一个形状的两种读法**，分开它们的是分号在不在。

**护栏**：`gaps.rs::a_macro_from_a_header_can_be_a_statement_of_its_own`——四种读得出的写法（`if` 之前、
`#endif` 之前、连着两次、`return` 之前）＋文件作用域两形（类体后的 `_GLIBCXX11_DEPRECATED_SUGGEST("…");`、
后面跟着声明）＋**三条边界**（`FOO(x)`、`g(x)` 换行、`g(x) { }` 都必须仍然报错）＋一条**次序**断言
（B73 那条仍然**是**一个声明、`MacroCall` 在说明符序列里，而不是"宏语句 + 另一条声明"）。

**量到的**：128 个文件的闭包 干净 109 → **111**、报错 19 → **17**、消息 119 → **91**；455 个文件的分析闭包
干净 426 → **429**、报错 29 → **26**、消息 147 → **129**。`move.h`、`stl_iterator_base_funcs.h`、`find.h`
变干净；**两份清单都没有一个文件从干净变报错**。`bits/stl_algobase.h` 的首错 239 → **906**（该文件只剩 8 条错）。

### B77. 枚举表里的指令（已修）与初始化式里的指令（**量过三版、撤回**）

```cpp
      bad_file_descriptor = EBADF,
#ifdef EBADMSG
      bad_message = EBADMSG,                     // x86_64-w64-mingw32/bits/error_constants.h:52
#endif
      broken_pipe = EPIPE,
```

**现象**：`expected ;` 打在枚举项那一行（`error_constants.h:53`，`#endif` 之后）。另一条同族的写法在
`ext/concurrence.h:58`：一个变量的值按分支写三遍，每个分支自带 `;`。

**成因（枚举表这一条）**：枚举项的循环里没有接缝——读到一个枚举项、一个逗号之后，下一个 token 若是 `#`，
循环就把它当成"列表结束了"，而列表其实还没结束。这与 B57（requires 体内）、B64（模板参数表）、
B69（形参表）是**同一条接缝**的第五、第六处。

**性质**：缺规则（接缝位置）。两处接缝：**逗号之后、枚举项之前**（`#ifdef`/`#else`/`#endif` 都落在这里），
以及**枚举项之后、逗号之前**（`b = 2` `#endif` `,` 这一形）。第二处是量出来的：只做第一处时，
`enum class E : int { a = 1, #ifdef X b = 2 #endif };`（最后一项没有尾逗号）仍报 `expected ;`。

**初始化式那一条：量过三版，全部撤回。** 那条接缝可以复用 `parse_a_definition_per_branch`（别名与 concept
规则已经用它读"每个分支一个定义"），但**最后那个 `;` 归谁**三家读者意见不一致，三版都不成：

```text
1  每个 `=` 都走那台机器         `int x = 1;` 成瓦砾：机器吃掉了分支的 `;`，上一层的声明读取器又要一个
2  ＋一个"这个 `;` 是初始化式的"     concurrence.h 修好了，**五个文件从干净变报错**：formatfwd.h、
   标志（`expect_semicolon` 取用）  nested_exception.h、cmath、aligned_buffer.h、type_traits.h——那个标志
                                 活过了设置它的那条声明，邻居于是不再要分号
3  ＋"只有 `#else`/`#elif` 跟在     同样五个文件再坏一次：别名与 concept 规则**依赖**旧答案来读它们自己的
   `;` 后面时才吃掉它"            分支
```

三版共同说明的是：**最后那个 `;` 是"声明的 `;`"，而共用这台机器的三个读取器对"谁拥有它"没有共识**——
这是 `parse_a_definition_per_branch` 的**设计问题**，不是缺一条规则，所以记在这里而不是硬修。
每一次的账都在 `%TEMP%\stdprobe\run_b77*.txt` 里（干净 426/25 与 429/25 的对照）。
**枚举表那条只值 1 个文件，两版都干净落地**——它的接缝与初始化式那条没有任何共用。

**护栏**：`gaps.rs::a_directive_may_stand_between_enumerators`——四种拼写（逗号后 `#ifdef`、`#else` 分支、
`#endif` 在最后一项与 `}` 之间、无指令的普通枚举）＋**形状**断言：三个枚举项**一个不少**，且文本各自正确
（接缝吞掉一个枚举项的树是干净的，正是这条断言要拦的）。

**量到的**：128 个文件的闭包不动（111 / 17 / 91——`error_constants.h` 在 MinGW 那一支，不在 libstdc++ 闭包）；
455 个文件的分析闭包 干净 429 → **430**、报错 26 → **25**、消息 129 → **126**。`error_constants.h` 变干净，
**没有一个文件从干净变报错**。

### B78. 复合要求里的花括号（`{ _Begin{}(__t) } -> bidirectional_iterator;`）—— 已修复

```cpp
template<typename _Tp>
  concept __reversable = requires(_Tp& __t)
	{
	  { _Begin{}(__t) } -> bidirectional_iterator;      // bits/ranges_base.h:214
	  { _End{}(__t) } -> same_as<decltype(_Begin{}(__t))>;
	};
```

**现象**：`expected }, but get {` 打在内层 `{` 上，而**要求体里它后面的每一条要求都成瓦砾**——`ranges_base.h` 一处
首错带 24 条诊断，`concepts` 同形（它的首错 `_Tp{};` 也在要求体里）。

**成因**：**约束里的 `{` 一律被当成"被约束的那个定义的身体"**，于是后缀循环拒绝把 `{` 读成表达式的一部分
（`requires C<T> { }` 若读成 `C<T>{}`，函数体就丢了——这条判据是对的）。但**复合要求自己的 `{ }` 里面**那个 `{`
不可能是身体：它已经嵌在要求的括号里一层了，读法就是"表达式后面的 `{`"（列表初始化的临时量，紧跟的
`(__t)` 再调用它）。

**性质**：判据**用在了一处它不适用的位置**（判据本身没错），所以修法是给它加一个例外而不是改判据：
`Requirement` 节点开着的期间，光标一定在复合要求的花括号里，`p.is_open(CppSyntaxKind::Requirement)` 就是
这个问题的答案——不需要再加一个标志。

**护栏**：`gaps.rs::a_compound_requirement_may_hold_a_braced_temporary`——六种读得出的写法（`_Begin{}(t)`、
`T{1}`、`{ t.f() } noexcept -> B`、同一个要求体里两条带花括号的要求、以及两种被约束的函数定义）＋**形状**断言
三条：`_Begin{}` 是 `InitListExpr` 且落在 `Requirement` 里；被约束函数的身体仍是 `CompoundStat`；
**类头仍然没有子句的位置**（`struct S requires C<T> { };` 照旧报错——那正是这条判据存在的原因，`tests/concepts.rs`
一直在钉它）。

**量到的**：128 个文件的闭包 干净 111 → **113**、报错 17 → **15**、消息 91 → **66**；455 个文件的分析闭包
干净 430 → **432**、报错 25 → **23**、消息 126 → **101**。`ranges_base.h` 与 `concepts` 变干净，
**两份清单都没有一个文件从干净变报错**。
### B79. 基类子句里的 `decltype`，与模板实参表**逗号两侧**的指令 —— 已修复

```cpp
  template<typename... _Bn>
    struct __or_
    : decltype(__detail::__or_fn<_Bn...>(0))          // type_traits:199——算出来的基类
    { };

    using __is_signed_integer = __is_one_of<__remove_cv_t<_Tp>,
	  signed char, signed long long
#if defined(__GLIBCXX_TYPE_INT_N_0)
	  , signed __GLIBCXX_TYPE_INT_N_0                   // type_traits:811——指令在逗号之前
#endif
```

**现象**：`expected a name` 打在 `:` 上（基类子句那一行），整个类头跟着坏掉；第二条报
`expected a template argument` 打在 `#if` 那一行。

**成因**：两处都是**规则只覆盖了一半**：
* 基类子句只调 `parse_name`，而标准的 base-specifier 是 `class-or-decltype`——`decltype(…)` 是另一半，
  而 libstdc++ 的 `__or_`/`__and_` 全部用这一半命名"算出来的基类"；
* 模板实参表**有**指令接缝，但只在**参数之前**（B64 做的），而 `#if` 也可以站在**参数之后、逗号之前**——
  循环这时回到的是逗号检查，不是参数读取，所以那道接缝永远看不到它。

**性质**：缺规则，两处各补一半；第二处与 B77 的枚举表**同形**（逗号两侧各要一道接缝），这正是"同一条接缝的
第二处"——第一次就该两边都做。

**护栏**：`gaps.rs::a_base_may_be_a_decltype_and_an_argument_list_holds_directives`——八种读得出的写法
（`decltype(d::f<int>(0))`、包展开版、`public decltype(0)`、三种普通基类子句、逗号前后的两种指令）＋
**形状**断言两条：基类子句里是 `TypeId` 而不是裸名字；逗号两侧有指令时**两个实参都在**
（接缝吞掉一个实参的树是干净的，正是这条要拦的）。

**量到的**：文件数一个没动（这条是把首错往后推的那一类）——128 个文件的闭包 消息 66 → **62**；
455 个文件的分析闭包 消息 101 → **97**。`type_traits` 的错 13 → **9** 条，首错 198 → 811 → **986**
（下一个形状是 `struct __is_signed_helper<_Tp, true>`，记在队列里）。**没有一个文件从干净变报错**。
### B80. 定位 `new` 的初始化式里有表达式（`::new (p) T(a.c())`）—— 已修复

```cpp
	::new (std::__addressof(_M_alloc)) _NodeAlloc(__nh._M_alloc.release());   // bits/node_handle.h:157
```

**现象**：`expected ), but get .` 打在初始化式里的 `.` 上（`node_handle.h:157` 那一条首错）。

**成因**：`new` 的类型读完之后，`parse_abstract_declarator` 里有一条分支把后面的括号组读成**函数类型**（那是
`new (Widget)(1)`、`sizeof(void(int))` 那一形的来源）。判据 `a_parameter_list_is_the_type` 只看**每个元素的第一个
token** 能不能开始一个形参——而 `(__nh._M_alloc.release())` 以名字开头，于是整组被认成形参表，形参读取器接着撞上
`.` 就报错。这些括号其实是**分配式的初始化式**，类型读完就该停在那里，由 `parse_new_initializer` 接手。

**性质**：判据**看得不够远**（与 B74 的"答案缺一条收尾"同族，但这里是判据本身的视野问题）。补法是：在元素级别上
出现**只有表达式才有的 token**（`.`、`->`、`+`、`/`、`%`、`|`、`^`、`~`、`||`、`==`、`!=`、`?`）就说明这一组不是
形参表。`-` **刻意不在**表里：`= -1` 是默认实参，形参表可以有。风险可以忽略——这条判据只在 type-id 里被问
（那里形参不能有名字、默认实参也没有意义）。

**护栏**：`gaps.rs::a_placement_new_initialiser_may_hold_an_expression`——八种读得出的写法（`T(a.b)`、
`T(a.c())`、`T(x + 1)`、`new N(a.c(), 2)`，以及判据本来要保住的 `::new (p) T(1)`、`sizeof(void(int))`、
`new (Widget)(1)`、`void g(int)`）＋**形状**断言两条：分配式的 `TypeId` 文本是 `T`（不是 `T(a.c())`），
且 `NewExpr` 里有一个 `Initializer`。

**量到的**：128 个文件的闭包 干净 113 → **114**、报错 15 → **14**、消息 62 → **60**；455 个文件的分析闭包
干净 432 → **433**、报错 23 → **22**、消息 97 → **95**。`node_handle.h` 在**两份清单**上都变干净，
**没有一个文件从干净变报错**。
### B87. 文件自己的 `#define` 体定读法：命名空间头与 `}` —— 已修复

```cpp
namespace std
{
inline _GLIBCXX_BEGIN_NAMESPACE_VERSION          // bits/c++config.h:401
#if __cplusplus >= 201402L
  inline namespace literals { … }
…
_GLIBCXX_END_NAMESPACE_VERSION                   // 同上:412
}
```

**现象**：`c++config.h` 五条消息，第一条是 ``expected `;` `` 打在 401 行那个 `inline` 上，其余四条都是它往下滚的结果。

**成因**：`_GLIBCXX_BEGIN_NAMESPACE_VERSION` / `_GLIBCXX_END_NAMESPACE_VERSION` 的体是 `namespace __8 {` 与 `}`，
而且**就在这个文件自己里**（393/394 行）。文件自己的 token 里没有一个字说"命名空间开了"或"花括号关了"，
于是 401 行的 `inline NAME` 被读成一条缺分号的声明，后面的声明全成了它的尾巴。**缺的不是证据，是没人读它**：
这是 B83 那张账单上第一条只用文件自己的材料就能做完的形态（另外四种——形参表片段、声明符的头、调用约定 +
指针声明符、属性——都还要索引）。

**做法**：`#define` 分支读完一行时把体的 **token kind** 记进 parser 侧的 `macro_bodies` 表
（`record_macro_body` / `macro_body_kinds`；名字与形参表都不进体），再由两条规则消费它：

* `body_shapes_the_braces`：第一个 kind 是 `namespace` ⇒ 这个调用**开了一个命名空间**；体恰是 `[}`]` ⇒ 它
  **关掉最里面那个花括号**。两个形状就是"头"与"尾"，其余形态不碰（`_GLIBCXX_MATH_NS` 的体是 `__8`，那是
  命名空间**名**而不是头，归声明符规则）。
* 读法：`MacroCall`（`parse_a_macro_invocation_statement`）。体是花括号的宏没有实参组、没有 body、没有分号，
  用组读取器（`parse_macro_call`）会**直接失败**——第一版就是这么错的，测出来的树里一个 `MacroCall` 都没有。
  `inline` + 宏这一形在**声明尝试之前**问：声明读法会先报一条缺分号，然后才轮到回退。

**空体不记**：`c++config.h` 在同一条 `#if` 的另一支里把这两个名字定义成**空**（423/424 行）。两支都会读到、
没有"选中哪支"这一说，所以空体不覆盖有形状的体——否则 393 行那条真定义会被 423 行抹掉，规则又哑了。

**护栏**：`gaps.rs::a_macro_whose_own_body_opens_a_namespace_is_its_own_statement`——零报错、**两个 `MacroCall`**，
外加反例 `#define NS __8` 必须一个都不产生（名字不是头）。

**量到的**：128 个文件的闭包 干净 114 → **115**、报错 14 → **13**、消息 60 → **55**；455 个文件的分析闭包
干净 433 → **434**、报错 22 → **21**、消息 95 → **90**。`c++config.h` 在**两份清单**上都变干净，
**没有一个文件从干净变报错**。

**代价与边界**（写下来，不当事没发生）：树在头与尾这两处是**平的**——`namespace __8 {` 的 `{` 不在文件里，
那层作用域没有真实的 `{`/`}` 可挂，规则只把调用读成一条语句，不假装嵌套。要真嵌套，粒度是
"**每个宏定义一棵树**"（见 [`index-design.md`](index-design.md) 的展开一节），不是这一刀。另外这里只问
**本文件**的 `#define`：头文件里的 `#define` 仍然要有索引才问得到，那是 B83 账单剩下的四种形态。

### B88. 模板形参表里的宏：分隔符与一整个形参都在头文件里 —— 已修复

```cpp
template<typename _Res, typename... _ArgTypes _GLIBCXX_NOEXCEPT_PARM>   // bits/refwrap.h:142
```

**现象**：``expected `,` or `>` in template parameter list`` 打在表尾那个 `>` 上。

**成因**：`_GLIBCXX_NOEXCEPT_PARM` 是 `, bool _NE`（`bits/c++config.h:269`）——**分隔符和一整个形参**都在头文件里，
本文件的 token 里没有一个字说表还要继续。这一条本该由"读宏的体"解决，体也确实取得到了（B86 的 `body_range`，
加上这一轮新落的**闭包 seeds**）；**量出来的坏消息是**：这两个 `#define` 落在
`#if __cpp_noexcept_function_type` / `#else` 两支里，也就是**条件定义**，而 seeds 的构造一直在跳过
`!FactGuard::Unconditional` 的事实（理由正当：`#if` 里的 `#define` 可能根本没跑，拿它当无条件事实喂进去就是
**扁平表那个错误换了个马甲**）。于是**闭包走完了，这个名字一条都没进来**——账单上剩下的宏恰好全是这一类。

**做法**：改由**位置**定读法。模板形参表读到"这里该写 `,` 或 `>`"的位置时，若那里是一个**写成宏样子的名字**
（`_` 开头或全大写，`types::written_like_a_macro`），就读成一次调用（`MacroCall`）并继续循环：体若供 `, 更多`
就继续，体为空则由下一个 token 关表，下一个 token 是 `>` 就地关表。

**为什么这条判据站得住**：位置本身已经把大部分选项排除了——真实形态是**参数包**（`typename... _ArgTypes`），
包名之后没有第二个名字的位置，所以那里出现标识符时，文件已经在"错"里了。边界也量过并写进了测试：**非包**
形参后面这个读取器**会**接受一个名字（`template<typename T foo>` 今天是干净的），所以那种形态不能当对照，
对照只能用包形态。

**护栏**：`gaps.rs::a_macro_may_supply_the_rest_of_a_template_parameter_list`——正例干净且**有一个 `MacroCall`**；
反例两条都用包形态：`typename... _Args foo` 与 `typename... _Args int` 都仍然报错。

**量到的**：128 个文件的闭包 干净 115 → **116**、报错 13 → **12**、消息 55 → **51**；455 个文件的分析闭包
干净 434 → **435**、报错 21 → **20**、消息 90 → **86**。**没有一个文件从干净变报错**。

**下一步（这一条量出来的）**：账单上剩下的形态需要的正是**条件那一层**——某个 `#if` 支是否成立、条件能不能
求值——而不是更多形状规则。料已经在库里（`ConditionAt` / `ConditionalRegion` / `preprocess::condition`），
缺的只是"把'这支成立'翻译成'这条 `#define` 可以当证据'"。细节写在 [`index-design.md`](index-design.md) 的展开一节。

### B90. 声明头由宏供给（`STDMETHOD(QueryInterface) (…) PURE;`）—— **已修复**（B97 之后有料了）

```cpp
DECLARE_INTERFACE_(IPrintDialogCallback,IUnknown) {      // commdlg.h:575
#ifndef __cplusplus
    STDMETHOD(QueryInterface) (THIS_ REFIID riid,LPVOID *ppvObj) PURE;   // :577
```

`combaseapi.h` 在 `__cplusplus` 那一支里（条件层会把它放进生效集合）写着：`STDMETHOD(method)` =
`virtual COM_DECLSPEC_NOTHROW HRESULT STDMETHODCALLTYPE method`、`PURE` = `= 0`、`THIS_` = **空**。

**现象**：`577:65 expected ';' after expression`，`commdlg.h` 的首错。

**量到的三件事**（都带工具，不是读代码猜的）：

1. **成员那一行本身能读**。`struct I { STDMETHOD(QueryInterface) (THIS_ REFIID riid, LPVOID *ppvObj) PURE; };`
   放进**类体**，今天就是零报错——第一版测试用它当正例，直接通过。
2. **真正卡住的是外层**。`DECLARE_INTERFACE_` 先被**声明说明符**那条路当成宏说明符吃掉（B73 的
   `a_macro_call_begins_the_declaration`），于是 `parse_declaration_here` 在整个文件里只在游标落到 `(` 上时被
   进入一次——插在它开头的钩子**永远不会在这个形状上被问到**（`B90 no: LeftParen […]` 的打印为证）。
3. **块因此被整块吞掉**，里面的成员行不是"语句"，语句层的钩子也不问它（同一份打印里，语句分发只在
   `VoidKeyword` / `LeftParen` 上出现过）。

**试过的做法（已撤回）**：`a_macro_head_with_a_parameter_list`（体以标识符结尾 ＋ 体里有只有声明才有的说明符
⇒ 这次调用是**声明的头**）＋ `parse_a_declaration_head_macro`（读成 `Declaration(MacroCall, ParameterList,
MacroCall(PURE), ;)`），分别挂在语句分发与 `parse_declaration_here`。**两份语料读数一条没动**
（455 那份 435/20/86、128 那份 116/12/51）——它**一次都没在语料上生效**，按纪律撤回：不改变任何读数的规则就是
没被量到的重量。一起撤回的还有它的两件使能件——`#define THIS_` 的**空体记录**（`macro_bodies_empty`）与形参表里
"空体宏跳过"——后者**同样没有可观测差别**：`void f(THIS_ int x);` 在没有宏表时今天也是零报错（这正是第二版测试
断言反例时量出来的）。

**下一刀切在哪（确切位置）**：不是声明层，是**说明符那一层**。`types.rs` 的 `a_macro_call_begins_the_declaration`
（B73）把宏调用读成声明说明符之后，那个块该按**类体**还是**函数体**读，应该由**体**决定：
`DECLARE_INTERFACE_` 的体是 `interface DECLSPEC_NOVTABLE iface : public baseiface`（有 `interface`、有基类
子句），而 gtest 的 `TEST(A, B)` 的体是语句/块。今天一律读成函数体（`set_last_declarator_is_function(true)`），
于是成员行成了语句、而语句里宏规则本来就受限制。下次先在那儿加一行打印确认块是谁读的，再按体分流。

**第二轮（把钩子挂对了，仍然撤回）——又量到三件事，都要留下**：

1. **钩子挂在哪**：语句块里的成员走的是 `parse_stat`，而它在 L223 有一条
   `_ if at_a_macro_call_statement(p) => parse_macro_call(p)`——**先于**声明/表达式那一问。所以文件自己
   `#define` 了 `STDMETHOD` 时（`di2.cpp` 那个复现），规则必须排在**那条臂之前**；从 include 拿到名字时则走
   声明/表达式那一问。两条路都挂上之后，`di2.cpp` 与 `symbols.rs` 的真实上下文测试**都能读**（
   `Declaration(MacroCall, ParameterList, MacroCall(PURE), ;)`）。
2. **规则会在语料上生效，而且会打坏两个文件**：`numbers`（`__glibcxx_numbers (_Float16, F16);`）从干净变报错、
   `combaseapi.h` 的首错从 358 行**提前**到 171 行（`DECLARE_HANDLE (CO_MTA_USAGE_COOKIE);`）。两个都是
   "体以标识符结尾 + 体内有声明专有说明符"，但**调用后面没有第二个括号组**——`parse_stat` 那条臂**没有回滚**，
   于是一条本来读得通的语句被吃掉了。**判据必须再加一条：实参组之后还要有 `(`**（那次没量到这一步就撤回了，
   下一次先加它再量）。
3. **`commdlg.h` 根本修不了——证据到不了它**：探针新增的 `| bodies in force: …` 打在每个失败文件的首错行上，
   `combaseapi.h:171` 那行显示 **`DECLARE_HANDLE` 有体在生效**，而 `commdlg.h:577` 那行**一个都没有**。原因是
   它的直接 include 只有 `winapifamily.h`、`_mingw_unicode.h`、`prsht.h`、`pshpack1.h`、`poppack.h`——
   **不含 `objbase.h`**，`STDMETHOD` 的体得从更远的一条链上传过来（或者那条 `#ifdef __cplusplus` 的体没被判成
   生效）。**这是下一轮的第一件事**：先量清楚闭包走法为什么没把 `STDMETHOD` 的体带到 `commdlg.h`，再谈读法——
   否则规则写了也是空转。

**撤回后的状态**：两份语料 128 那份 **116/12/51**、455 那份 **435/20/86**，`cargo test --workspace` **1066 个
测试 / 34 个套件**全绿（新增的 `symbols.rs` 那条测试按"能力落地那天会失败"的写法留着：两条路今天都读不出来，
断言的是"读不出来"）。探针那条 `bodies in force:` 是这一轮的**工具**收获，它把"证据没建"与"证据没用"分开了。

**第三轮：落地了（B97 把料送到之后）。** 规则本体与两处钩子照旧，**判据多了一条**——正是第二轮量出来的那一条：
这次调用后面**还得有 `(`**（`kind_after_the_balanced_group(p, index) == Some(LeftParen)`）。理由写在判据的注释里：
`DECLARE_HANDLE(CO_MTA_USAGE_COOKIE);` 与 `__glibcxx_numbers(_Float16, F16);` 同样是"体以名字结尾、被调用"，
第一版把这两条也claim了，语料从干净 435 掉到 432；声明头后面跟的是**声明符**，所以必须有第二个括号组。

**量到的（带 seeds，也就是带索引的产品形态）**：455 那份 干净 436 → **437**、报错 19 → **18**，逐文件对照
**只有 `commdlg.h` 变化**、**没有一个文件反向**（`commdlg.h` 就是这一族追了四轮的那个文件）；128 那份 116/12/51 不动；
不带 seeds 的普查 435/20/86 不动——规则要吃体，体只有带索引那条路才有。

**护栏**：`symbols.rs::a_declaration_head_whose_name_is_the_macros_argument`——两条路各断言"零报错 + 恰好一个
`ParameterList`"（文件自己的 `#define`、以及从环境来的体），外加两条反例：`DECLARE_HANDLE (X);`（没有第二个括号组）
与 `MAXIMUM(1, 2) (3)`（体是表达式）都必须**不**产生 `ParameterList`。

### B91. 宏的体是**说明符**（`(_CONST_RETURN wchar_t *)(_S)`）—— 已修复

```cpp
return (_CONST_RETURN wchar_t *)(_S);        // wchar.h:1461
… (unsigned __LONG32) …                      // basetsd.h:88-90
```

**现象**：`wchar.h:1461` 的首错是 ``expected ), but get identifier``，打在那行 `wchar_t` 上——括号里的 type-id 读到
一半就断了。

**成因**：`_mingw.h:376` 在**生效的那一支**里写的是 `#define _CONST_RETURN`——**空体**（`const` 那种拼法在另一支
里）；`basetsd.h:16` 的 `POINTER_32` 同样是空体，`__LONG32` 是 `long`。文件自己的 token 里没有一个字说"这里什么
都没有"或"这里是一个说明符"，于是 type-id 把 `_CONST_RETURN` 读成一个名字、把后面的 `wchar_t` 当成多余的名字。

**做法**：`parse_decl_specifier_seq` 的循环里加一条——**体全是说明符的宏**按说明符读（读成 `MacroCall`，文件自己的
token 一个不动）；体里含 `long`/`int`/`unsigned` 这类**真的命名了类型**的 token 时，它同时算"已命名类型"
（`const` 只算说明符，于是 `wchar_t` 仍是那个类型）。体来自 B89 的两条通道：文件自己的 `#define`，或闭包带进来的。

**空体只在 type-id 里当"什么都没有"**（`!allow_second_name`），这条边界是**量出来的**：把空体也接受在**声明**的
说明符序列里，语料从干净 435 掉到 **424**、消息 **617**——因为声明里跟在后面的是**声明符的名字**，被吞掉就整条
声明没了。type-id 里没有名字可丢，所以那里安全。测试把这条边界钉住了（`symbols.rs`）。

**量到的**（**带 seeds 的普查**，也就是带索引的产品形态）：455 那份 干净 435 → **436**、报错 20 → **19**、
消息 86 → **84**，只有 `wchar.h` 变化，**没有一个文件反向**；128 那份仍是 116/12/51。**不带 seeds 的普查一条不动**
（435/20/86）——这条规则要吃体，而体只有带索引的那条路才有：**两份读数要一起看，别把"没带索引"当成"没效果"**。

**顺带量到的一件大事（写给下一次）**：**头文件的宏可能来自"包含它的那个翻译单元"的顺序**。`commdlg.h` 自己的
include 链是 `winapifamily.h` / `_mingw_unicode.h` / `prsht.h` / `pshpack1.h` / `poppack.h`——**没有 `objbase.h`**；
`STDMETHOD` 之所以在那里可见，是因为 `windows.h:108` 先 `#include <objbase.h>` 再 `#include <commdlg.h>`。
所以**单文件闭包在结构上就到不了它**——B90 那条规则修不了 `commdlg.h`，不是规则写错了。要修得像 LSP 那样按
**翻译单元**喂环境（compile database，或"谁包含这个头"）。探针的 `MACRO <名> in <文件> …` 与首错行上的
`bodies in force:` 就是为这类问题准备的。

### B114–B118. 收尾的五条：**按分支写的尾巴**、别名上的属性宏、扫描里的 `;`、限定名的头、表达式里的实现关键字 —— 已修复

这五条落地之后，**两份带 seeds 的读数都是零错误**：455 那份 **455/0/0**、128 那份 **128/0/0**（不带 seeds 的 455 是
454/1，剩下的那个是 `commdlg.h` 的 `STDMETHOD`——它**必须**有闭包的体，带 seeds 的读数里它是干净的）。

```text
B114 声明/别名/条件的**尾巴**按分支写（形参表的 `… ) ;`、模板实参表的 `… > ;`、三目的 `: expr ;`，
     以及**终结符共享**的那种：分支只写 `… )`，`{` 在 `#endif` 之后）→ algorithmfwd.h、stl_iterator.h、
     type_traits.h（2274）三个文件修好
B115 别名名字与 `=` 之间的**属性宏**（`using aligned_storage_t _GLIBCXX23_DEPRECATED = …`）→ type_traits.h 下一层
B116 两处扫描里**配对花括号内的 `;`** 不再是边界（`bool_constant<!requires(…) { __f(__t); }>`）→ type_traits.h 再下一层
B117 `the_declaration_has_a_type` 要问**token 级**的"头是不是限定名"（变量模板的偏特化
     `bool __detail::__is_subrange<subrange<…>> = true;`）→ ranges_util.h **修好**
B118 `__extension__` 站在**操作数位置**（`__ret = __extension__ _S_nd<unsigned __int128>(…)`）→ 128 那份最后一个
```

**B114（按分支写的尾巴）是这一批的主体，也是唯一一次动"归属"的**。四份形状、一条规则：

```cpp
    random_shuffle(_RAIter, _RAIter,                       // parallel/algorithmfwd.h:700 —— 形参表，分支带 `;`
#if __cplusplus >= 201103L
		   _RandomNumberGenerator&&);
#else
		   _RandomNumberGenerator&);
#endif

    void random_shuffle(…,                                 // bits/stl_algo.h:4600 —— 同一形状，**终结符共享**
#if __cplusplus >= 201103L
		   _RandomNumberGenerator&& __rand)
#else
		   _RandomNumberGenerator& __rand)
#endif
    { … }

  using __iter_key_t = remove_const_t<                     // bits/stl_iterator.h:3090 —— 实参表，分支带 `;`
#ifdef __glibcxx_tuple_like
      tuple_element_t<0, typename iterator_traits<_It>::value_type>>;
#else
      typename iterator_traits<_It>::value_type::first_type>;
#endif

    return __len > (…) ? _Max_align::value                 // type_traits:2269 —— 三目的 `:` 支，分支带 `;`
# if _GLIBCXX_USE_BUILTIN_TRAIT(__builtin_clzg)
	     : 1 << (__SIZE_WIDTH__ - __builtin_clzg(__len - 1u));
# else
	     : 1 << (__LLONG_WIDTH__ - __builtin_clzll(__len - 1ull));
# endif
```

规则一句话：**本支里的 `)`/`>` 只是"这一支的写法"的结尾**，它后面的 `;` 是这一支的；当那个 `;` 之后是 `#else`/`#elif`
**并且**这个构造开始之后有**条件被打开**，下一支的尾巴就按同一个构造读。三件事缺一不可：

① **归属**用的是 B77 学到的那一课：`parse_a_definition_per_branch` 已经把答案放在**返回值**里（"`;` 是不是在这里吃掉的"），
而不是放在一个会活过声明的标志里。这一批照做，但形态更轻：一个**取用式**标志
（`note_the_terminator_came_from_a_branch` / `take_the_terminator_came_from_a_branch`），由 `expect_semicolon` 取走。
**它进 `Checkpoint`**——与 `open_bodies` 相反，因为投机读法**真的会**经过设置它的读取器：语句级的"先试声明"在每个
可能是表达式的语句上都会跑一遍形参表，回滚之后标志必须跟着回滚，否则下一个声明就会跳过自己的 `;`。

② **两道门的第二道是"谁打开了那个条件"**，而它**不能**由构造自己数：`type_traits:2269` 的 `# if` 是**真分支里的表达式**
在算符位置的接缝上吃掉的，构造自己根本没看见它。所以计数放在**读指令的那一处**（`parse_preprocessor_directive` →
`note_a_directive_name`），构造只记下开始时的值再比较（`CppParser::open_conditionals`）。第一道门是形状：
`;` + `#else`/`#elif`，**或者**尾巴后面直接跟 `#else`/`#elif`（终结符共享的那种）。
**这道门是被量出来的**：只看形状时，`#if X void f(int a); #else void g(long a); #endif` 会被读成"一个形参表、两个
实参"，`type_traits:1177` 与 `stl_iterator.h:3023` 就是这么冒出来的。

③ **同一批 token 谁先看见谁说了算**这一课又来了一次：`a_conditional_opens_here` 原来按**token 种类**判断指令名，
而 `#if`/`#else` 的 `if`/`else` 是 **C++ 关键字**（`IfKeyword`/`ElseKeyword`），种类判断把它们全漏掉；改成按**文本**
判断，并用**行尾偏移**挡住"空指令 `#` 读到下一行的 `if`"。

**量到**（带 seeds）：451/4 → **455/0**，消息 8 → **0**；128 那份 122/6 → **128/0**。断言：
`gaps.rs::a_declarations_tail_may_be_written_once_per_branch`（三种拼法各一条 + 终结符共享那条 + "一个构造不是一支一个"的
计数 + 反例"分支各写整条声明 ⇒ 两个形参表"）、以及把两个**过时的反例**（`assert_does_not_read_yet` 里钉着 B65 与
stl_iterator 那条的两处）改成**正例**——这是这一批最好的证据：那两个"读不出来"的钉子被拔掉了。

**B115–B118**（每条都是"偏好缺一条证据"的老形状，各值一个首错）：
① 别名名字与 `=` 之间的属性**宏**（attribute 关键字那条早就有，宏这条用声明符后缀的同一个读者
`eat_a_macro_suffix`）；② 数尖括号的扫描与"类头有没有 body"的扫描都把 `;` 当边界，但只在**深度 0** 才是——匹配花括号
里的 `;` 是 lambda / requires-expression 体里的语句（`type_traits:3946` 的基类子句里正好有一个 requires 表达式）；
③ `the_declaration_has_a_type` 原来只问**记录下来的**限定名标志，而 `A::f<int>` 这条拼法**记录是空的**（名字以实参表
结尾，找回名字的走法停在 `>` 上）——那个函数自己的文档就写着这件事，于是补上 **token 级**的那一问
（`the_head_of_the_declaration_is_qualified`）；④ `__extension__` 站在操作数位置，按实现关键字跳过、再读操作数
（实现关键字表是既有的 `an_implementation_keyword`，新加的只是一个布尔谓词，免得把它的私有返回类型暴露出去）。

**剩下的一个**（不带 seeds 455 那份：`commdlg.h:577` 的 `STDMETHOD(QueryInterface) (…) PURE;`）**不是缺陷**：那个
读法要 `STDMETHOD` 的体，而体在 `combaseapi.h` 里——只有带索引的那条路才有，所以带 seeds 的读数里它是干净的。
这正是"两份读数必须分开报"的最后一条注脚。

### B107–B113. 从 445/10 压到 451/4 的**七条**（两条成对修好四个文件）—— 已修复

这一批的共同点：**每条都是"偏好写好了，缺的是一条证据或一次统一"**，而不是缺一条规则。逐条如下，每条都带**量到的**。

```text
B107 初始化式/表达式**按分支写**：`#else` 之后跟着另一个值、运算符位置的指令、
     括号里的 `#endif`。→ bits/stl_algobase.h、tr1/riemann_zeta.tcc **两个文件修好**
B108 一条 cast 规则，两处副本：一元规则的副本在"没有操作数"时报错，主规则的副本回退成括号表达式
     → iterator_concepts.h **修好**
B109 属性站在 declarator-id 与**形参表**之间（`operator== [[nodiscard]] (…)`）→ tuple **修好**
B110 说明符位置上的函数式宏，**组后面跟的是名字**：那个名字要么本身是说明符宏，要么不是声明符
     → stl_function.h 的第一道门
B111 形参表**之后**的限定符也是"这是函数声明符"的证据 → stl_function.h **修好**
B112 无参宏 + 块，以及体内一个**裸的**无参宏语句（`SCOPE_BEGIN { … } SCOPE_END`）
     → debug/safe_iterator.h **修好**
B113 数尖括号的扫描里的**三目 `:`**（`sub<A, B, (c) ? x : y>`）→ ranges_util.h 的第一道门
```

**B107（按分支写的值与项）**：`bits/stl_algobase.h:906` 把初始化式写在两条分支里（`#else` 之后是**另一个值**，`;` 在
两支之外），`tr1/riemann_zeta.tcc:179` 把乘积的**项**写在两条分支里（两支各写同一个 `*`，`#endif` 之后表达式还继续），
`bits/stl_algobase.h:1237` 则是在**括号里**连着两个 `#if` 块各贡献一个操作数。三处都是同一件事：**指令站在表达式里**。
做法：① `finish_init_declarator` 的 `Assign` 分支在初始化式**之后**按 `#else`/`#elif` 再读一个值（B100 那两个接缝的
延伸，`;` 仍然归声明）；② 表达式爬升算符循环在**算符位置**读指令——但**只在这条指令后面真的跟着算符时**才收，
否则回滚，因为 `#else` 开的是**包着这个表达式的那层构造**的另一个实例（B107① 要的正是它）；③ 指令后面不是算符时，
`#else`/`#elif` 交还给外层，其余（`#endif`、下一个 `#if`）由表达式自己收下，因为后面等着的 `)`/`,`/`;` 正是调用者要的。
**量到**：带 seeds 445 → **447**（stl_algobase.h、riemann_zeta.tcc 干净）、消息 35 → 28。

**B108（一条 cast 规则）**：`(V<T>(x))`——类型读法把 `V<T>(x)` 当成**函数类型**、`)` 也对上，然后**没有操作数**。
表达式语法里这条规则有**两处副本**（一元规则与主规则），而它们对这件事的答案相反：主规则回退成括号表达式，
一元规则的副本直接报"缺操作数"。于是同一批 token 谁先看见谁说了算：`(V<T>(x))` 成瓦砾、`(W<T>(x))`（名字不认识，
守卫说"不是类型"）却干净。删掉一元那份，保留有回退的那份。**量到**：446 → **448**（iterator_concepts.h 干净）。

**B109（属性在 id 与形参表之间）**：`bool operator== [[nodiscard]] (int, int);`——声明符的后缀循环见到 `[[` 就
**停**（那个位置本来是留给 `finish_init_declarator` 的属性读法的），于是形参表没人读。做法：后缀循环里，`[[` 之后
**若跟着 `(`** 就由声明符自己读（属性和形参表都留在 `Declarator` 里，`ParameterList` 的父节点才对）；否则照旧交给
`finish_init_declarator`。判据是一个只问"属性串后面是不是 `(`"的扫描。**量到**：448 → **449**（tuple 干净）。

**B110/B111（stl_function.h 的两道门）**：`_GLIBCXX17_DEPRECATED_SUGGEST("std::not_fn")` 之后跟的是**名字**
`_GLIBCXX14_CONSTEXPR`——B102 那条臂的后继判据只收"说明符"，于是这条形状归了 `a_macro_call_begins_the_declaration`
（它读完调用就结束序列）。B110 把后继放宽成"**名字，且这个名字后面不是 `(`**"（后面是 `(` 说明它是声明符，归原来那条）；
B111 修的是同一个文件的下一个形状 `mem_fun(_Ret (_Tp::*__f)(_Arg) const)`——形参表**之后**的 `const` 是"这是函数声明符"
的证据，而那条偏好只认 `{` 和 `->`，于是回退到初始化式读法、`const` 留在原地。做法：问"限定符读取器**动过游标**吗"
（不另写一份 token 列表）。**量到**：449 → **450**（stl_function.h 干净）。

**B112（宏 + 块，以及体内裸的无参宏）**：`debug/safe_iterator.h:75-79` 把两个作用域宏定义成 `[&]() -> void` 与 `();`
（另一支是**空**），用法是 `if (…) SCOPE_BEGIN { … } SCOPE_END else …`。名字后面跟 `{` 本来是 C++11 的
**列表初始化**（`Point{1, 2};`），于是块里的每条语句都进了 `InitListExpr`。做法：① 语句规则里加"**有证据的无参宏 +
块**"——`MacroCall(NameExpr, CompoundStat)`，证据是两张表知道这个名字（同 `at_a_macro_call_statement` 的问法）；
② 这条读者继续收**跟在块后面的**收尾宏（`SCOPE_END`），否则 `if` 的分支在 `else` 之前就结束了；
③ 体内**裸的**无参宏语句（`} SCOPE_END`）由 `a_macro_invocation_starts_at` 的新分支认领：证据（表说它是宏、可无 `;`）
+ 后继（`ends_a_statement`，其中已有 `else`/`catch`/`}`）。**量到**：450 → **451**，消息 21 → **9**。

**B113（三目里的 `:`）**：`bits/ranges_util.h:436` 的推导指引把**条件表达式**当模板实参
（`-> subrange<…, (sized_range<_Rng> || …) ? sized : unsized>`），而"这个 `<` 是不是实参表"的扫描把 `Colon` 当停止符
（那是给基类子句用的）⇒ 扫描说"不是模板 id" ⇒ 指引在自己的名字上停住，首错打在返回类型的 `<` 上。做法：扫描里数
**未配对的三目 `?`**，有它就放行那个 `:`；没有 `?` 的 `:` 仍然停。**量到**：451 → 451（消息 9 → **8**，ranges_util.h 的首错
从 438 推到 **484**——下一个形状是变量模板的偏特化 `bool __detail::__is_subrange<subrange<_Iter, _Sent, _Kind>> = true;`）。

**剩下的 4 个**（带 seeds 451/4、消息 8）——**三个是同一族**：

```text
ranges_util.h:484   变量模板的偏特化，名字里那个实参是**模板形参自己**（`<sub<I, S, k>>`，`k` 是 `K k` 声明的）
                    —— 判据已缩到一行：head 里的非类型形参名出现在名字的实参表里（s1/s3 失败、s2/s4/s5/s6 干净）
algorithmfwd.h:704  形参表的**尾巴**按分支写：`…, _RandomNumberGenerator&&);` / `#else … &);` —— 分支里带着 `;`
                    （B77 的归属问题）
stl_iterator.h:3094 别名模板实参表的**尾巴**按分支写：两支各写 `<实参> >;` —— 同上
type_traits:2274    三目的 `:` 支每支写一遍，每支带自己的 `;` —— 同上
```

后三个是**同一件事**：一个构造的分支里带着末尾的 `;`，也就是"**谁拥有分支末尾那个 `;`**"——B77 记着三种试法各自的
代价（`;` 是分支的还是声明的，三个读者不同意），而 B100/B103/B105/B107 之后**接缝这一侧已经补齐**，剩下的正是归属。

### B106. `*` 与名字之间的**属性**（`void * __attribute__((…)) f (void) { }`）—— 已修复

```cpp
extern __inline void * __attribute__((__gnu_inline__, __always_inline__, __artificial__))
__slwpcb (void) { … }                       // lwpintrin.h:43 —— 整个 `*intrin.h` 家族都这么写
void * __attribute__((x)) f;                // 同一个形状，没有函数体
```

**现象**：带函数体时首错是 ``a declarator takes only one initializer``（`{`，45:0）；**不带**函数体时**完全没有报错**——
这才是要紧的那一半：

```text
void * __attribute__((x)) f;
  Declaration(DeclSpecifierSeq(void), InitDeclarator(Declarator(* __attribute__ (x)), MacroCall(f)))
                                        └─ 声明符的名字叫 `__attribute__`，`(x)` 是它的初始化式，`f` 成了一个宏
```

**成因**：`eat_cv_qualifiers`（`*`/`&`/`&&` 之后的那个位置）读了 `const`/`volatile`、实现关键字、**已知空体宏**，
唯独没读**属性**。而它自己的文档里写着这就是"说明符位置那条规则，晚一个 token"——属性正是写在那里的东西之一。
于是属性被当成声明符的名字，真正的名字留在后面成了"替声明站位的宏"（B72 那一类 A0 静默错树）。

**做法**：`eat_cv_qualifiers` 里加一条属性臂（`at_an_attribute` → `parse_attribute_specifier`，失败则回滚，与
`eat_a_macro_suffix` 同一处理）。

**量到的**（带 seeds）：干净 444 → **445**、报错 11 → **10**，逐文件对照**只有 `lwpintrin.h` 消失**、无一反向；
`lwpintrin.h` 现在**整份文件一个错都没有**。断言：`gaps.rs::an_attribute_may_stand_between_the_star_and_the_name`
（正例要求恰好一个 `AttributeList` + 一个 `ParameterList` ⇒ 它是函数；**静默那一半**要求零个 `Initializer`、零个
`MacroCall`；反例是同一个位置的 `const`——它是限定符，不能被数成属性）。

### B105. 条件里的声明**必须在条件的 `)` 上结束**（`if (NS::fmod(s, T(2)) == 0)`）—— 已修复

```cpp
if (_GLIBCXX_MATH_NS::fmod(__s,_Tp(2)) == _Tp(0))       // tr1/riemann_zeta.tcc:173
```

**现象**：``expected ), but get ==``，打在那个 `==` 上。

**成因**：条件的**声明读法成功了**，而且是通过它自己的两道判据"合法"地成功的：

```text
if (NS::fmod(s, T(2)) == 0)
    └────┬────┘ └──┬──┘    `NS::fmod` 是限定名 ⇒ 是类型；`(s, T(2))` 是函数声明符
         │         └────── `T(2)` 被读成形参 `T` + 直接初始化式 `(2)` ⇒ "有初始化式" ✓
         └──────────────── 形参里的 `s`、`T` 是 NameExpr ⇒ "解析出了名字" ✓
```

两条判据问的都是**声明符自己的事件**，而形参表也产生这些事件。于是条件读成了一个函数声明，接着条件要它的 `)`，
撞上 `==`。

**做法**：加第三条拒绝——声明之后**必须**是条件的 `)`。条件的声明没有自己的 `;`，所以 `)` 就是"声明结束"的唯一标志；
不是它就不是条件声明，按 [`parse_condition`] 的既定偏好退回**表达式**（调用）。

**量到的**（带 seeds）：干净 444 → 444、报错 11 → 11、消息 39 → **36**；`riemann_zeta.tcc` 的首错 173 → **182**
（下一个形状是"表达式按 `#if` 分支写"——`__zeta *= A * B` / `#if X * C #else * D #endif / E`，又是 B77 那一族）。
**不算修好**，与 B99/B104 同一类：读法正确、断言有、队列前移。断言：
`gaps.rs::a_condition_declaration_ends_at_the_paren_that_closes_the_condition`——正例是那条调用（要求树里**零个**
`Initializer`，因为误读产出的正是一个"带直接初始化式的形参"），反例是三种**真的是**声明条件
（`if (Foo* p = get())`、`if (const auto n = g())`、`if (Foo p = get())`），每一种都必须仍然声明出它的变量。

### B104. 模板实参那一族的**三处读法**（都读对了，但一个文件都没修好）—— 读法已修，计数未动

```cpp
C<(__i >= sizeof...(_Types))>                        // tuple:2439     —— 括号里的 `>=`
struct S : public C<_Tp(-1) < _Tp(0)> { };           // type_traits:987 —— 实参里的 `<`
return c ? a
# if X
         : b;                                        // type_traits:2269 —— 三目两支之间的指令
# else
         : c;
# endif
```

**三处改动**（都在"实参表/类型扫描"这一层，都是**读法**的修正）：

① **失败的读法不是答案**（`parse_template_argument`）。原来那条早退是
`type_read.is_ok() || !continues_a_type(当前 token)`：**失败**的 type 读法（`(__i` 读到 `>=` 放弃）也被当成"实参读完了"，
而它消费掉的 token 还留在事件流里。于是 `>=` 落到实参表循环里被 split 成 `>` + `=`，`>` 收尾，声明没了头（首错
``expected a declarator name`` 打在 `=` 上）。改成**只有成功的 type 读法**才能早退；`continues_a_type`（只有这一个调用者）
随之删掉——"停在能结束类型的地方"这件事由 `current_token_index() > start` 那半句负责，不需要第二次判断。

② **`>=` / `>>=` 不是收尾符**（`get_operator_precedence`）。实参表里原来的拒绝集是 `>`/`>>`/`>=`/`>>=`；g++ 的读法是
`>` 与 `>>` 才收尾，`>=` 一律当运算符（`C<sizeof(int) >= 4>`、`C<(1) >= 2>` 都接受，`C<1 > 2>` 才是"第一个 `>` 收尾"）。
拒绝集收窄到 `>`/`>>` 之后，`__enable_if_t<(__i >= sizeof(...))>` 的括号里那个 `>=` 才读得成运算符；而"类型实参后面直接跟
`>=`"（`C<D<int>= 3>`）仍然是 `split_closing_angle` 的活（g++ 对它报的是 "`>=` should be `> =`"，我们读得更宽容）。

③ **数尖括号的三处扫描合成一处，并且 `<` 要看前一个 token**（`angle_depth_delta`）。三个扫描（
`a_matching_angle_bracket_follows`、`a_bare_template_id_is_here`、`a_body_follows_the_class_head`，外加两个小的）各自
数自己的尖括号，而**同一个 `angle_depth_delta` 现在多收一个参数**：前一个 token 能不能结束一个模板名
（标识符 / `>` / `>>`）。因为组里的 `<` 是**小于号**：

```text
::std::vector<int>    `<` 前面是名字   → 开一层
C<1 < 2>              …前面是字面量    → 小于号
C<sizeof(T) < 3>      …前面是 `)`      → 小于号
C<(T(0) < T(0))>      …在组里          → 小于号
```

把组里的 `<` 数成开层，后果是**扫描自己在结尾差一层**，于是"找不到匹配的 `>`" ⇒ 那个 `<` 被当成小于号 ⇒
基类子句整段不成模板 id、类头"没有 body"、实参表整个不成立——三处扫描在 `type_traits:987` 那一行上**同时**错。
g++ 对上面三行的读法正是"后三行是实参、第一行是内层列表"。

**量到的**（带 seeds，455 那份）：干净 **444 → 444**、报错 11 → 11、消息 41 → **39**。也就是说**一个文件都没修好**，
但两个文件的首错各往前走了，而且都撞在**同一个刻意不读的形状**上：

```text
tuple       2438（expected a declarator name，B104① 修掉）→ 2532 `operator== [[nodiscard]] (参数表)`
type_traits  987（expected `;`，B104③ 修掉）→ 2271 `# if` 打在 `:` 上（B104 的三目接缝）→ 2274
```

`type_traits` 停在 2274 是**分支写法**：三目的 `:` 那一支**每个 `#if` 分支写一遍**，每支带自己的 `;` ——
这就是 B77 记着"三种试法各自的代价"的那个形状（一个定义写在两个分支里）。所以这一条**不算修好**，
只算"读法正确、断言有、队列往前挪了两格"，与 B99 同一类。

**边界与断言**：`gaps.rs::a_template_argument_may_be_a_comparison_in_parentheses`（tuple 的括号比较 + 四个基类实参
逐字形 + `BaseSpecifier` 计数）、`gaps.rs::a_conditional_may_be_written_with_a_directive_before_its_colon`
（三目接缝正例 + "`?` 之前的指令不是这条接缝的"反例）。

### B103. 枚举量名字与 `=` 之间的**宏**（`omp_proc_bind_master __GOMP_DEPRECATED_5_1 = …`）—— 已修复

```c
  omp_proc_bind_master __GOMP_DEPRECATED_5_1          // omp.h:74 —— 宏定义在本文件的两条分支里
    = omp_proc_bind_primary,
```

**现象**：``expected primary expression`` 打在下一行的 `=` 上（75:4）。

**成因**：枚举量的名字与 `=` 之间那个位置**只读了 `[[…]]`**（属性），没读宏。宏被当成名字之后，枚举量在那里就结束了，
循环要 `,` 或 `}` 却撞上 `=`，整个 `typedef enum … } omp_proc_bind_t;` 从那一行起成瓦砾。

**做法**：那条属性读法旁边加 `while eat_a_macro_suffix(p) {}`——同一个读者（"名字 + 可选实参组"），同一个理由：
枚举量名字之后合法的 token 只有 `[[`、`=`、`,`、指令、`}`，**名字一个都不是**，所以没有第二种读法要争。

**量到的**（带 seeds）：干净 443 → **444**、报错 12 → **11**，逐文件对照**只有 `omp.h` 消失**、无一反向。
断言：`gaps.rs::an_enumerator_may_carry_a_macro_before_its_value`（正例 + 两个枚举量 + 一个 `MacroCall`；
反例是**同一个位置的 `[[…]]` 拼法**——它必须继续工作，而且不能被数成宏）。

### B102. 声明说明符位置上的**函数式属性宏**（`_GLIBCXX11_DEPRECATED_SUGGEST("std::bind")`）—— 已修复

```cpp
template<typename _Operation, typename _Tp>          // backward/binders.h:133
  _GLIBCXX11_DEPRECATED_SUGGEST("std::bind")
  inline binder1st<_Operation>
  bind1st(const _Operation& __fn, const _Tp& __x)
  { … }
```

**现象**：首错 ``expected `;` ``，打在那段字符串字面量上（134:33）。

**成因**：`_GLIBCXX11_DEPRECATED_SUGGEST` 是**函数式**宏，体是 `_GLIBCXX_DEPRECATED_SUGGEST(ALT)` →
`__attribute__((__deprecated__(ALT)))`。B91 那条"体全是说明符"的规则读不到它——**函数式宏的体里有自己的形参**，不是说明符表；
`at_an_attribute` 也不认它（体在别的文件里）。于是说明符序列在这个名字上停住，声明从它的 `(` 开始散架。

**这一条量了三个版本**，两个失败的版本正是判据的来源：

```text
要求 evidence（is_function_like）     语料一条没动 —— binders.h 根本没有 evidence 可要求
只看形状（不加任何表判据）            两个文件**反向**：objbase.h:95、ole2.h:58（都是 WINOLEAPI_(…) 那一族）
要求"两张表都说不出这个体" + 后继     干净 442 → 443、报错 13 → 12，只有 binders.h 消失、无一反向 ← 留下的这一版
```

第一行的原因是探针自己打出来的，值得抄在这里：`backward/binders.h` **一个 `#include` 都没有**（"internal header,
included by other library headers"），单独读它（普查就是这么读的）时**每张表都是空的**：

```text
MACRO _GLIBCXX11_DEPRECATED_SUGGEST in binders.h   evidence false | positional body None
                                                   | in-force body None | context 0 seeds
```

第二行的原因是**两条证据通道是分开的**：`WINOLEAPI_` 的 `#define` 在 `_mingw.h` 的**条件分支**里，所以它到使用点时
是"**生效的体**"而不是"定义"——只问 `macro_evidence` 仍然会把它算成"没人知道"，于是抢走了
`a_macro_call_begins_the_declaration` 那条规则的形状。两张表都问，这条规则就只剩下**别人读不了**的名字。

**做法**：`parse_decl_specifier_seq_with` 里加一条臂（紧挨着 B91 那条），条件是
① `specifiers == 0`（序列开头）② 名字后面是括号组 ③ 不是属性拼法 ④ `macro_evidence` 与 `macro_body_kinds_at`
**都**说不出这个体 ⑤ 组**后面跟着一个"说明符"**（不是名字）。参数按**原始 token** 收
（`parse_balanced_token_group`）：`("std::bind")` 是什么语法，只有另一个文件里的 `#define` 知道。

**第 ⑤ 条是这一版的骨头，也是被量出来的**：组后面跟**名字**的形状（`MACRO(args) Name (…)`）归下面的
`a_macro_call_begins_the_declaration` 所有——它读完调用就**结束序列**（"宏展开成什么不可知，后面那个名字既可能是声明符
也可能是类型里的又一个词"）。这一版先写成"名字或说明符都收"，结果 `WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);`
（今天能读的形状）变成**调用表达式** + ``expected `;` after expression``——序列把 `CoFreeLibrary` 并进了宏所代表的类型。
所以名字那一半被**明确排除**，两半各归各的规则，`gaps.rs` 里既有正例（invocation + `inline` + 类型 + 名字）
也有反例（`CHECK(1);` 在函数体里必须**仍然是调用**：没有这条判据时它会读成"没有声明符的声明"——无声、无损、且用户写的调用
在树里根本不存在）。

**边界**：这只在**声明说明符序列的开头**、且**两张表都沉默**时成立。表里有东西时（`WINOLEAPI_`）走的仍是原来的规则；
类体里"整个成员就是一个调用"的形状由 `at_a_macro_member` 先claim。

### B101. 括号声明符里的**宏**：`(*MACRO NAME)` 与 `(MACRO *NAME)` —— 已修复

```cpp
typedef void (*_GLIBCXX11_DEPRECATED unexpected_handler) ();                              // exception:87
typedef HRESULT (STDAPICALLTYPE *LPFNGETCLASSOBJECT) (REFCLSID, REFIID, LPVOID *);        // combaseapi.h:358
```

**现象**：前者 ``expected ), but get identifier``（87:17），后者 ``expected `;` ``（358:16）。两个宏的体都**在生效**
（`_GLIBCXX11_DEPRECATED` 是 `__attribute__((__deprecated__))`、`STDAPICALLTYPE` 是 `__stdcall`）。

**成因**：`(MACRO NAME)` 这条路**早就有了**（`winperf.h` 的 `(WINAPI PM_OPEN_PROC)`），缺的是**宏相对声明符自己的运算符
的另外两个位置**：`STDAPICALLTYPE` 站在 `*` **前面**，`_GLIBCXX11_DEPRECATED` 站在 `*` 与名字**之间**。判据
`a_parenthesised_declarator_with_a_name_follows` 两处都不认 ⇒ 整个括号组被当成参数列表/函数类型读，声明在自己的括号里散架。

**做法**：① 判据加两条（`(* MACRO NAME)`、`(MACRO * NAME)`），都要求**那个宏**写得像宏
（`written_like_a_macro`，不是要求名字）；② 读取器 `parse_parenthesised_declarator` 里补上"**运算符之前的宏**"这一半：
`parse_abstract_declarator` 之后、名字之前，若当前是"像宏的名字 + `*`/`&`/`&&`"，先读成一个 `MacroCall`，再继续读抽象声明符。
**这一半正是 B98 撤回时缺的那一半**（那时判据与读取器都写了，错误从 col 16 推到 col 51 却没修好——因为宏在 `*` 前面那一半没人读）。

**量到的**（带 seeds）：干净 440 → **442**、报错 15 → **13**，逐文件对照**只有 `exception` 与 `combaseapi.h` 两个消失**、
无一反向。**不带 seeds 的这一条与 B102 一起量**（两次改动之间没有单独量过，别把它当成 B101 一个人的数）：
439 → **442**、报错 16 → **13**，逐文件对照消失的三个正是 `exception`、`combaseapi.h`、`binders.h`（B102 的）、无一反向。

### B100. 声明初始化式的**两侧接缝**，与 `__attribute` 少一个下划线的拼法 —— 已修复

```cpp
// ext/concurrence.h:58 —— 一个变量的值写在三条分支里，`#ifndef` 站在 `=` 与值之间
_GLIBCXX17_INLINE const _Lock_policy __default_lock_policy =
#ifndef __GTHREADS
  _S_single;

// bits/stl_algobase.h:906 —— 值写完之后，`#endif` 站在值与声明自己的 `;` 之间
const bool __load_outside_loop =
#if __has_builtin(__is_trivially_constructible) \
      && __has_builtin(__is_trivially_assignable)
    __is_trivially_constructible(_Tp, const _Tp&)
    && __is_trivially_assignable(__decltype(*__first), const _Tp&)
#else
    __is_trivially_copyable(_Tp)
#endif
    ;

// parallel/compatibility.h:48 —— GNU 的属性拼法**少一个下划线**也是拼法，不是名字
extern "C"
__attribute((dllimport)) void __attribute__((stdcall)) Sleep (unsigned long);
```

**现象**：前两个文件的首错都是 `expected primary expression`，**打在指令那一行**（907:0、59:0）；第三个文件是
`expected \`;\``，打在它自己的 `extern "C"` 上。

**成因**：两件事，互不相干。

① **接缝**：`=` 之后、值之前（以及值之后、`;` 之前）这个位置没有读指令的接缝。枚举量、模板形参表、形参表、花括号初始化
式都各自有这一条（B23 的九种形状），唯独声明自己的 `=` 漏了。表达式规则没有 `#` 的读法，于是 `Initializer` 规则直接报
"expected primary expression"——**报错的位置是指令，不是声明**，这也是探针把这四个文件归成"指令位置"一族的原因
（另外两个 `omp.h:75`、`lwpintrin.h:45` 是别的成因，见下面的清单）。

② **拼法**：`at_an_attribute` 只认 `__attribute__` 与 `__declspec`。libstdc++ 自己的 `parallel/compatibility.h`
写的是 `__attribute`（**一个**尾下划线），`g++ -std=c++17` 接受这一行 —— 也就是说这是**编译器的扩展拼法**，与
`__attribute__` 同义，判据正是 `at_an_attribute` 文档里那两条（名字归实现保留 + 扩展属于编译器而不是文件）。

**做法**：① `finish_init_declarator` 的 `Assign` 分支里，`parse_initializer_clause` **前后各加一个指令环**，两个环都在
`Initializer` 节点**里面**（指令是"这个初始化式怎么写"的一部分，与枚举量的值同理）；② `at_an_attribute` 的拼法表加
`__attribute`。

**量到的**（455 那份，带 seeds）：干净 438 → **440**、报错 17 → **15**；**不带 seeds**：干净 437 → **439**、
报错 18 → **16**（"不带 seeds" 的基线是**临时把这两条规则关掉**重新量的，不是拿旧运行凑的——push1–4 都是带 seeds 的）。
两种模式**逐文件对照**只有 `concurrence.h` 与 `compatibility.h` 两个消失、无一反向：

```text
 59:0   expected primary expression  | #ifndef __GTHREADS                    :: concurrence.h      → 干净
 49:11  expected `;`                  | __attribute((dllimport)) void …       :: compatibility.h    → 干净
907:0   expected primary expression  | #if __has_builtin(…) \                :: stl_algobase.h     → 首错推到 912:5
```

`stl_algobase.h` **没修好**，但首错**往后走**了（两种模式都是 907:0 → 912:5）：接缝读进去以后，撞上的是另一个已知
形状——`#else` 分支里的值是**同一个声明符的第二个初始化式**，也就是 B77 特意不读的"一个定义写在两个分支里"
（那一节记着三种试法各自的代价）。这一条不动。

**边界**：接缝只在 `=` 之后与 `InitDeclarator` 的 `Assign` 分支内，声明符之后**不**读指令——
`gaps.rs::a_directive_may_stand_on_either_side_of_a_declarations_initializer` 用 `int x\n#if 1\n…` 钉住
"指令不会凭空造出一个初始化式"，另用 `\` 续行的 `#if` 钉住两个环都真的读到了指令。

### B98. 指针限定符位置上的**空体宏**（`(void *POINTER_32) p`）—— 已修复

```cpp
#define POINTER_32                                        // basetsd.h:16 —— 64 位目标上是**空体**
static __inline void *POINTER_32 PtrToPtr32 (const void *p) { return ((void *POINTER_32) (ULONG_PTR) p); }
```

**现象**：`basetsd.h:90` 的首错是 `expected primary expression`，打在那个 cast 上。

**成因**：空体宏在**文件自己的** `#define` 里没有被记录（B90 把那一半撤回过），于是规则问不出"这个名字展开成什么"，
cast 的 type-id 在 `*` 之后撞上一个名字就断了。

**做法**：① 文件自己的空体 `#define` 记进 `macro_bodies_empty`，**与有形状的体分开存**（空体顶不掉有形状的体——
`bits/c++config.h` 在两条分支里分别把同一个名字定义成 `namespace __8 {` 和空，两支都要读）；② `macro_body_kinds_at`
对它们回答 `Some([])`——"什么都没有"，与 `None` 的"没人说过"是两回事；③ 消费者只有一处，而且是**窄**的：
`eat_cv_qualifiers`，也就是 `*` 后面的限定符位置（`* const`、`* __ptr32`）——那里一个空体宏只能是什么都没有。

**量到的**（带 seeds）：455 那份 干净 437 → **438**、报错 18 → **17**，逐文件对照**只有 `basetsd.h` 变化**、
无一反向；不带 seeds 的 435/20/86 不动。**边界没动**：空体宏在声明的说明符序列里仍然不被接受（B91 量过：535 → 424 那一版）。

**B98 附带：试过、量过、撤回的那半（`(STDAPICALLTYPE *NAME)`）。** `combaseapi.h:358` 写
`typedef HRESULT (STDAPICALLTYPE *LPFNGETCLASSOBJECT) (REFCLSID, REFIID, LPVOID *);`，而探针显示 `STDAPICALLTYPE`
的体**在生效**（`WINAPI` → `__stdcall`）。判据（`(MACRO * NAME)` 归括号声明符）与读取器都写了，错误从 col 16
推到 col 51（`expected a name` 打在后面的 `(REFCLSID…`）——**没修好**，语料一条没动，因此**撤回**：不留没量到的重量。
**下一刀的确切位置**：读掉宏之后的**名字登记**——`(MACRO * NAME)` 里的名字要像既有的 `(MACRO NAME)` 那条路一样被
登记成声明符的名字（后者是干净的，前者不是，差别就在这里）。

**剩下的 17 个，按族看**（这是"压到 10 个"的下一步清单，每条都带**已经量过的**切入点）：

```text
模板实参那一族（3 个文件，**三个互不相同的成因**，别当成一条）
  A. ranges_util.h:267  默认实参里的三目：`parse_a_default_value` **先试 type-id**，`sized_sentinel_for<_Sent>`
                        读成类型就返回了，`? :` 留给参数表 ⇒ 撞上"expected , or >"。
                        **已修（B99）**：type-id 只在"表还能继续"时才算答案（`,`/`>`/`>>`/`#`），否则同一批 token 按
                        表达式读；`gaps.rs::a_template_parameter_may_default_to_a_conditional_expression` 钉住正例与
                        "type-id 仍然赢"的反例。**语料读数没动**——那个文件的 267:6 是**带环境才出现**的首错（用
                        cpp_dump 直接读该文件时首错在 437 行），成因与这三条复现不同，要等实参表读取器的"为什么拒绝"
                        打印才能继续
  B. tuple:2438         **已修（B104①②）**：`__enable_if_t<(__i >= sizeof...(_Types))>` 的两道门——失败的 type 读法
                        被当成完整实参（`(__i` 停在 `>=`），以及实参表里 `>=` 被拒成运算符。两道都改掉后复现干净；
                        首错推到 **2532**（`operator== [[nodiscard]] (参数表)`，B77 那一族的下一层）
  C. type_traits:987    **已修（B104③）**，而且成因比上面写的更准：三处数尖括号的扫描都把 `(_Tp(-1) < _Tp(0))` 里的
                        `<` 数成开了一层 ⇒ 扫描在结尾差一层 ⇒ "找不到匹配的 `>`" ⇒ 这个 `<` 被当成小于号，
                        基类子句整段不成模板 id（类头的 `a_body_follows_the_class_head` 同时在"有没有 body"上错）。
                        复现矩阵（`template<typename T> struct S : public C<…> { };`）：修好后
                        `C<T(0)>`、`C<T(0) == T(0)>`、`C<1 < 2>`、`C<T(0) < 3>`、`C<sizeof(T) < 3>`、
                        `C<(T(0) < T(0))>`、`C<(sizeof(T) < 3)>` **全部干净**。首错由此推到 **2271**（三目两支之间的
                        指令，B104 的三目接缝）、再推到 **2274**（`:` 支每支写一遍 ⇒ B77 那一族）
指令位置（4 个）           ~~stl_algobase.h:907~~、~~concurrence.h:59~~、~~omp.h:75~~、~~lwpintrin.h:45~~ **四条全部结清**：
                        `concurrence.h` 由 B100 的 `=` 两侧接缝修掉、`omp.h` 由 B103 的枚举量宏修掉、
                        `lwpintrin.h` 由 B106 的"`*` 与名字之间的属性"修掉；`stl_algobase.h` 的首错从 907:0
                        推到 **912:5**，停在刻意不读的形状上（`#else` 分支的值 = 同一个声明符的第二个初始化式）
属性宏 + extern "C"        compatibility.h:49 —— **B100 已修，成因不是三轮探针缩到的那一处**：那一行里的
                        `__attribute` 只有**一个**尾下划线，`at_an_attribute` 不认这个拼法 ⇒ 它被读成宏形说明符，
                        后面的声明跟着丢。`g++ -std=c++17` 接受同一行，拼法属于编译器而不属于文件；加进拼法表后该文件干净
单条                       ~~binders.h~~（B102）、~~exception~~、~~combaseapi.h~~（B101）、~~omp.h~~（B103）、
                           ~~lwpintrin.h~~（B106）已修；剩下 iterator_concepts.h、stl_function.h、stl_iterator.h、
                           safe_iterator.h、algorithmfwd.h、riemann_zeta.tcc、tuple、type_traits、
                           stl_algobase.h（B77 那个刻意不读的形状）
```

**这一族按"值多少文件"的下一刀**（带 seeds **455/0**、128 那份 **128/0**——两份都是零错误；不带 seeds 的 455 是
454/1，剩下的是必须有闭包才成立的那一条）：

```text
1. 没有剩下的成族形状了。带 seeds 的语料**零错误**，而 455 那份不带 seeds 的读数里唯一失败的文件
   （commdlg.h:577 的 STDMETHOD）不是缺陷：它的读法要 STDMETHOD 的体，而体在 combaseapi.h 里
2. 下一步不是"再修一个文件"，而是换清单：**把 index-design.md 的索引队列往前推**（本节记的都是 parser 的账），
   或者拿一份**更大的闭包**（整份 libstdc++ + Windows SDK）来量——现在这两份清单已经量不出东西了
```

### B42. 函数定义里的 `try`（function-try-block）—— 待修

```cpp
void f() try { } catch (...) { }        // `try` 写在声明符与函数体之间
S::S() try : m(1) { } catch (...) { }   // 构造函数版本，构造函数初始化列表在 `try` 之前
void f() try { } catch (X& x) { }       // 处理器可以带参数
```

**现象**：四个报错，第一个落在 `void` 上（`expected primary expression`），然后是 `{`、`catch`、结尾的 `}`。
整条定义读不成函数定义。

**成因**：`try` 作为**语句**有规则（所以同样的 token 写在函数体**里**完全正常），但"声明符与函数体之间"
这个位置没接：`parse_a_definition_per_branch` 读完声明符就等 `{`，`try` 不在它的 follower 里。

**性质**：缺规则，接缝位置——和维护约定第 31 条同族（"向回走的判据要知道什么包着这个构造"）
以及 B40（`try` 的两个 `#if` 接缝，已修）是同一处构造的第三、第四种写法。

**护栏**：`gaps.rs::constructs_the_parser_does_not_read_yet`（`Where::File`）钉住它现在读不出来，
能力落地那天这条会失败——那正是有意为之。

### B43. 条件里的声明（`if (int x = g())`）—— 已修复（见 B47）

```cpp
if (int x = g()) { }        // C++ 的 condition 可以是 declaration
while (const auto& v = next()) { }
switch (int n = f(); n) { }
```

**现象**：`expected primary expression` 落在 `int` 上，随后块的大括号被当成瓦砾
（`unexpected token`、`expected ; after expression`）。

**成因**：条件的读取只有表达式这一条路；`condition` 在标准里是
`declaration` 或 `expression` 二选一，而"先试声明、失败再试表达式"这个范式 parser 在别处已经用了很多次
（维护约定第 32 条讲的就是它的陷阱：失败的试探**不回退游标**）。

**已修复**（第十七轮，见 **B47**）：`parse_condition` 现在调 `parse_condition_declaration`——说明符 + **一个**初始化
声明符，而且**必须有初始化式**（否则 `if (a && b)` 会被读成"把 `b` 声明成 `a&&`"，这是第一版真正的坑），
逗号列表一律拒绝。本条留在这里是因为它的**成因分析**是对的（二选一 + 回退），而 B47 是它的落地。

**性质**：缺规则（一条二选一的读法）。**不是**恢复的问题——虽然症状长得像（块被吃掉），
那个方向查过了：`parse_compound_stat` 对缺 `}` 的处理是对的。

**护栏**：`gaps.rs::a_condition_may_declare_a_variable`（读得出 + `condition_is_a_declaration` 的形状断言 +
"`if (v.size())`/`if (i++)`/`if (a && b)` 仍然不是声明"的反面）。

B42 与 B43 都是**在修恢复的时候顺手量出来的**：做法是拿 53 段**合法的** C++ 片段过一遍 parser，任何报错都值得
看一眼（见维护约定第 36 条最后一句话）。三条候选里有一条是**我们对了、片段写错了**
（`sizeof (T) (x);` ——g++ 也拒绝它），"拿编译器验一遍片段"因此写进了那条约定。B43 已经落地，B42 还在队列里。

### B41. 宏调用**省略分号**，而宏体自带语句 —— 已修复（改动是**建宏表**，不是放宽判据）

```cpp
#define NUMBER_OPTION(op) if (auto v = Get(op); !v.empty()) { … }

NUMBER_OPTION(indent_size)      // 宏体是一条完整语句，于是调用处不写分号
NUMBER_OPTION(tab_width)
```

**现象**：`LuaStyle.cpp` 剩下的 **64 条**全是它：`NAME(args)` 后面紧跟下一条语句（或 `}`），报 `expected ; after expression`，随后级联（23 条落在宏调用行、41 条是级联）。

**成因**：严格说这是**不合法的 C++**——调用表达式后面必须有 `;`——但宏体自带分号时，预处理后的代码是合法的，而 parser 不做预处理（这个立场本身是对的，见 B23）。所以这不是"缺规则"，而是"要不要为宏再开一次口子"。

**关键观察（决定了修法）**：这三个宏**都 `#define` 在同一个文件里**（`LuaStyle.cpp:27/29/39`）。于是"要不要开口子"这个二选一根本不必做——**能拿出证据的才算宏**：

* `#define` 过 ⇒ 收下（`BOOL_OPTION(x)` 不写 `;` 是宏的用法）；
* 没有 `#define` ⇒ 照旧报错（`g(x)` 漏分号就是漏分号）。

**修复**：新建 `parser::MacroNames`（形如 `TypeNames`：文件局部、有上限、只记名字不解释宏体），在 `parse_preprocessor_directive` 里收 `#define` / `#undef`；语句层新增一条规则 `at_a_macro_call_statement`——游标是标识符 + 下个 token 是 `(` + **表里有这个名字** ⇒ 读成新的 `CppSyntaxKind::MacroCall`（名字 + 平衡 token 组 + 可选的块），宏体自带什么就收什么：块收进节点、`;` 收掉、**什么都没有也照样成立**（这正是本条的用例）。

**与拼写约定的分工**（这是本轮最重要的一条设计判断）：**需要证据的地方用表，兜底的地方用约定**。语句层的"省略分号"会吞掉真手误（`g(x)` 漏 `;`），所以**只认表**；而"宏 + 块"（B32/B36）在文件作用域上本来就没有别的读法、体内也只有拼错才冲突，所以那里保留"宏拼写"兜底——头文件里来的宏（gtest 的 `TEST`、glib 的一堆）依然读得出来。

**代价与收获**：表只知道本文件（和 `TypeNames` 同一边界），头文件宏仍走约定；换来的是**可列举、可反向钉住**的放行范围——`gaps.rs` 的"仍不支持"清单里钉着"没有 `#define` 的名字漏写 `;` 仍然是错误"，哪天放宽了那条测试就会失败。

**复验**：`EmmyLuaCodeStyle` **200 个文件全部 0 报错**（本轮开始是 1 文件 / 64 条；最初是 66 文件 / ~650 条），`luajit-dll` 8 个 C 文件仍全绿。

**护栏**：`tests/macros.rs`（5 条：定义过的宏可省分号 + **三条反例**、`#undef` 撤销证据、宏体是块/是 `;`、实参是原始 token、表只管本文件）+ `gaps.rs` 已支持清单两条与"仍不支持"一条。

### B19. `sizeof` 的操作数只要不是裸名字就失败 —— 已修复
```cpp
sizeof(a[0]);        // expected ), but get [
sizeof(a.b);         // expected ), but get .
sizeof(a + b);       // expected ), but get +
sizeof(a());         // expected ), but get (
typeid(a[0]);        // 同上
sizeof(int);         // 一直能读
sizeof(unsigned long);  // 一直能读
```

**性质**：缺规则（一个条件不够严）。**成因**：`sizeof(...)`/`typeid(...)`/`alignas(...)` 共用的 `parse_type_id_or_expression` 先试类型读法，判据是"解析成功**且**消费了 token"。问题是 **type-id 可以提前收尾**——一个名字本身就是完整的 type-id。于是 `sizeof(a[0])` 的类型读法读完 `a` 就成功返回，游标停在 `[` 上，调用方报 `expected )`。**除了裸名字和关键字类型，所有 `sizeof` 操作数都失败**，而这在 C 里是多数。

**修复**：判据补上"读完之后游标必须停在 `)`"——载荷以 `)` 结束，没走到它的 type-id 就不是这次读法。真是类型的那种情况（`sizeof(unsigned long)`、`sizeof(int*)`）照旧走类型读法，其余回退给表达式规则。

**发现经过**：把探针从 `main.c` 扩到同目录的 `extensions.c` 时撞上的（`sizeof(names) / sizeof(names[0])`、`(int)(sizeof(...))`）。**同一个项目里换个文件就多挖出一条**——见维护约定第 11 条。

**护栏**：`expressions.rs::sizeof_reads_an_operand_that_is_not_a_bare_name`（16 条 + 截断操作数仍报错）。

### B20. `sizeof` 的两种类型操作数 —— 已修复

```cpp
sizeof(struct S);    // 曾报 expected primary expression —— elaborated 型说明符（C 里最常写的类型拼法）
(struct S*)p;        // 曾报 expected ) —— 同上
using A = struct S;  // 曾报 expected `;` —— 同上
sizeof(int[4]);      // 曾报 expected primary expression —— 数组类型
sizeof(int*[4]);     // 曾报 —— 指针数组
Vec<int[4]> v;       // 曾报 —— 数组类型作模板实参
sizeof(a[0]);        // 一直是表达式（下标），现在仍然必须是
```

**成因（两半是同一个入口的两个问题）**：两个操作数都要求 type-id 读法能吃下它们。

**第一半：elaborated 型说明符。** `struct S` 是**一个**说明符（elaborated-type-specifier），但 specifier 序列在 type-id 里只允许**一个名字**（那是给 `T x` 留的额度），而额度已经被 `struct` 这个关键字花掉了——于是 `S` 被拒，type-id 只剩 `struct`，载荷永远到不了 `)`。修法是在 `name_joins_the_type` 里加一条：**前一个被消费的 token 是 class 类关键字时，这个名字无条件加入类型**。判据用现成的 `last_consumed_token_kind()`，不加字段、不加状态。

**第二半：数组类型。** 抽象声明符读完之后，type-id 还要读**数组后缀**（`[4]`、`[2][3]`、`[4]` of `int*[4]`）。三个决定：

1. **只在 type-id 里读，不在抽象声明符里读**。抽象声明符与**声明符**共用，而声明符路径上一个 `[` 可能是**结构化绑定**（`auto [a, b] = pair`）——在那里读数组后缀会把结构化绑定吃掉。
2. **判据是"前面那个类型是本文件能证明的类型吗"**：关键字类型（说明符序列产出了 `BuiltinType`）✓，本文件声明过的类型名 ✓，裸的未知名字 ✗。`sizeof(int[4])` 与 `sizeof(a[0])` 是同一串 token，只有名字查找能分开；答不上来时**不动括号**，于是表达式读法（下标）自然接管——这是安全方向，也是 B19 那条"类型读法必须停在 `)`"能继续管用的原因。
3. **`new` 不读**。标准把 `new int[4]` 的界放在 **new-declarator** 里而不是类型里，`parse_new_declarator_suffixes` 就是读它的规则。type-id 若抢先吃掉，`ArrayType` 会从分配表达式的声明符里搬到 `TypeId` 里——**一个不会被任何报错发现的树形变化**。所以新入口 `parse_type_id_for_an_allocation` 明确关掉这一半（它的另一个参数 `a_name_may_be_a_type = false` 也是 `new` 特有的，理由见函数文档）。

**调试中发现的两处连带问题**（都不是设计的一部分，而是做的时候撞出来的）：

* **守卫问错了对象**：第一版守卫用 `last_consumed_token_kind()` 判断"前面是不是类型"，而抽象声明符可能刚吃掉一个 `*`——`sizeof(int*[4])` 于是被判成"前面不是类型"。修法是把这个问题**在说明符序列刚跑完时**就问掉（读它产出的 `BuiltinType` 事件），把答案作为参数传下去。**又是"第一个/最后一个 token 被当成了整条声明"**（维护约定第 9 条）。
* **`a_matching_angle_bracket_follows` 把 `]` 当成了边界**：它的停表里有 `RightBracket`，于是 `Vec<int[4]>` 里的 `<` 被判成"没有配对的 `>`"，模板实参读法根本没被尝试。修法是给扫描加一个**括号配对计数**：`[` +1、配对的 `]` -1，**只有落单的 `]` 才结束扫描**——`a[b < c]`（停表存在的理由：那里的 `<` 是下标里的比较）仍然正确。

**护栏**：`expressions.rs::an_array_type_is_read_as_a_type_and_an_index_as_an_expression`（8 条类型读法 + 4 条模板实参 + 形状断言：`sizeof(int[4])` 有 `ArrayType`、`sizeof(a[0])` **没有**而有 `IndexExpr`、`new int[4]` 的 `TypeId` 文本恰好是 `"int"`）；`direct_init.rs` 的 elaborated 与 cv 两组；`gaps.rs` 已支持清单 15 条 + 形状断言 5 条。

### B21. `a < b || c > d` 被读成 template-id，然后**报错** —— 已修复

```cpp
if (a < b) { }               // 一直读得出
if (a < b || c > d) { }      // 曾报 expected a template argument
x = a < b > c;               // 曾报 expected `;` after expression
if (n < 0 || n > 100000)     // 真实文件里的那一行
```

**性质**：缺规则（判据"成功"的定义不够严）。**成因**：`a < b || c > d` 里的 `<` 被 `could_start_template_arguments` 认成模板实参表的开头——它一路找到 `>` 就认为配对成功，**中间的 `||` 不在停表里**。于是 `a<b || c>` 成了 template-id。

**这条推翻了 T2 的判断**。T2 当初记的是"读成 template-id 是**良性**的：token 都还在，只是当成 template-id 而不是比较"。**不成立**：它不产生"另一个读法"，它产生**报错**。真实文件里 `if (n < 0 || n > 100000)` 就是这么挂的——比较表达式里出现 `||` 是家常便饭，而 `||` 正好把两个 `<`/`>` 连成了一个"模板实参表"。

**修复**：把"`<` 之后是不是模板实参表"从**一次提交**改成**一次带验证的试读**，判据分两层：

1. **试读失败** → 回退成比较。这修的是 `a < b || c > d`：实参读法在 `||` 上失败（类型读法把 `b` 当完整实参停下，列表循环回到 `||` 上要第二个实参）。
2. **试读成功，但后面跟着一个操作数** → 同样回退。这修的是 `n < 0 || n > 100000`：实参 `0 || n` 读得通、`>` 正常收尾，**整个读法是"成功"的**，只是后面挂了个 `100000`。**"解析成功"不是证据**——`template-id` 后面跟一个操作数在 C++ 里根本不成句，与"`)` 后面跟操作数"（T1）是同一条推理，所以判据复用 `starts_an_operand`。

lookahead **保留，但降级为"快速否"**：`a < b;` 这种常见比较不值得先建一棵实参树再丢掉。正确性不再依赖它准不准——它接受的一切都要过上面两层——所以以后也不必再为它调停表（往停表里加 `||` 会误伤 `Foo<A || B>`，那是合法的非类型实参）。

**名字分支与成员访问分支都要改**：`a.b < c > d` 是同一个歧义长在 `.b` 后面，回退点取在名字之后、`<` 之前，所以成员访问那半留在原地。

**一处刻意的例外**：**在 clause 内部，第二层不生效**。clause 后面跟着它约束的**声明**，而声明以类型开头——`template <typename T> requires C<T> T value = T{};`、`requires C<T> std::vector<int> v;`。把 `C<T>` 还回去，clause 就会读成 `C < T`，然后拿声明自己的类型当比较的右操作数。代价见下条。

**护栏**：`operators.rs::a_less_than_between_two_names_is_a_comparison`（11 条比较读法、断言 `TemplateArgumentList` 为 0、`a < b > c` 的形状是三个 `BinaryExpr`）、`operators.rs::a_genuine_template_id_keeps_its_reading`（12 条模板读法，另一侧）；`concepts.rs::a_constraint_keeps_its_template_id_when_a_declaration_follows`（5 条约束后的声明）；`gaps.rs` 已支持清单 11 条 + 形状断言 2 条。

---

### B6. 别名中的包展开

```cpp
template <typename... Ts> using T = std::tuple<Ts...>;   // 已修复
using T2 = decltype(f(args...));                          // 未知，大概率同构
```

已在 C2 的修复中一并解决；此处保留条目是为了记住 `using` 右侧那条路径与模板实参不是同一处代码。

### B7. 括号表达式与 lambda —— 已修复

```cpp
x = (a + b);                         // expected primary expression
x = (a > b) ? a : b;                 // 同上——三元运算符其实早已实现
auto g = [](int x) { return x; };    // expected primary expression
```

**性质**：缺规则，而且是**两处公共入口各缺一条分支**。

**成因一**：`parse_primary_expr` 里**没有 `LeftParen` 分支**，所以凡是**以 `(` 开头**的表达式全部失败。长期没被发现是因为能走到那里的括号形式，都是 `if (x)`、`f(a)` 这类**语句层**括号，由语句规则和后缀规则自己消费掉了。

**成因二**：三元运算符**早就实现了**（`parse_ternary_expr`），但 `(a > b) ? a : b` 得先解析括号里的条件，于是被成因一挡住。**`a ? b : c` 能过而 `(a > b) ? a : b` 不能**——这个不对称就是线索。

**成因三**：lambda 的 `[` 与下标运算符同形，需要**形状判据**而非一条分支：扫到 `]`，再看它**后面**是什么——`(`（参数）、`{`（body）、`mutable`/`noexcept`/`->`（限定符）才是 lambda，其余归下标读法。

**修复**（`exprs.rs`）：
1. `parse_primary_expr` 新增 `LeftParen` 分支，产出 `ParenExpr`——括号**保留**，不去掉；
2. 新增 `starts_a_lambda`（形状判据）与 `parse_lambda` / `parse_capture_list` / `parse_capture`，新节点 `LambdaCaptureList`、`LambdaCapture`；
3. `eat_function_qualifiers` 补 `mutable` / `constexpr` / `consteval`——lambda 的限定符与成员函数的限定符处在同一位置。

**顺手发现**：`ParenExpr`、`TernaryExpr`、`LambdaExpr` 三个节点类型一直在 kind 表里，但**没有任何规则产出它们**（`TernaryExpr` 有一个到不了的分支）。kind 表里有节点而规则里没有产出，是"承诺了但没兑现"——和 C1 里 `RequiresKeyword` 那张空头支票是同一类问题。

**仍不支持**：`(struct Point){.x = 1}` 复合字面量，以及 T1 里残留的非指针 cast。

---

## C 类：报错拒收，成本高

### C1. concept 与 requires —— 已修复

```cpp
template <typename T> concept C = requires(T t) { t.f(); };   // expected a type specifier @22..29
template <typename T> void f(T t) requires C<T> { }            // expected `;` @34..42
void f() { if constexpr (requires { g(); }) { } }              // expected primary expression @25..33
template <typename T> concept C = true; template <C T> void f(T t);  // expected a type specifier
```

**性质**：缺规则。**成因**：requires 表达式有四个子规则（simple / type / compound / nested requirement），可以任意嵌套，还需要接 requires-clause 和约束参数。

**一张空头支票兑现了**：`RequiresKeyword` 早已在 `is_expression_keyword` 列表里（`exprs.rs`）。那个列表是一张**承诺**——"有规则会消费这个 token"——而 requires 没有任何规则消费它，所以它只会让名字分支绕开它，然后让 `_` 兜底报 `expected primary expression`。现在 `parse_primary_expr` 有了对应的分支（见维护约定第 7 条）。

**另外注意**：`template <Number T>` 这种**受约束模板参数是好的**，`template <C T>` 也是。坏的只有 `concept` 声明本身和 requires-表达式。这两件事容易混为一谈——修完之后仍然是两件事：受约束参数走的是**类型名**那条路，与 clause 无关。

**修复**（一次做完整块，因为四件事共用同一个词）：

| 位置 | 规则 | 节点 |
|---|---|---|
| `template <C T>` | 已有的类型名读法，**未改动** | `TemplateParameter` |
| 模板头之后（`template <…> requires C<T> void f();`） | `parse_requires_clause` | `RequiresClause` |
| 函数声明符之后（`void f() requires C<T> { }`） | 同一个 `parse_requires_clause` | `RequiresClause` |
| 表达式位置 | `parse_requires_expression`，四个子规则在读 | `RequiresExpr`、`Requirement` |
| `concept C = X;` | `parse_concept_declaration` | `ConceptDecl` |

**子规则在语法里的位置是查过标准才写下的**（[temp.pre]、[dcl.decl.general]），两处结论与直觉不同，也因此改掉了实现里的一处**静默错树**：

```text
template-head:    template < template-parameter-list > requires-clause_opt
init-declarator:  declarator requires-clause function-contract-specifier-seq_opt
```

1. **类头没有 requires-clause**。标准里根本没有这个位置，`struct S requires C<T> { };` 是错的。原先实现把它当第四种"标准位置"读了，而**读出来的树是错的**：
   ```text
   Declaration(TemplateDecl, DeclSpecifierSeq(StructDef(S)), InitDeclarator(RequiresClause))
   CompoundStat { }        <- 类体掉到了文件作用域，成了一个"复合语句"
   ```
   良构、无损、零报错——A0 类。判据不是"解析了吗"而是"**类体和类还在一起吗**"——这正是第 6 条约定说的形状断言。现在 clause 只接在**函数声明符**之后（`last_declarator_is_function`，相邻分支早就在用的标志），`requires` 原样留下报 `expected ;`，类体留在类里。
2. **clause 在尾随返回类型之后，不在之前**。`auto f(T t) -> bool requires C<T>;` 对，`auto f(T t) requires C<T> -> bool;` 错——标准在 [dcl.decl.general]/5 的例子把这条与"clause 属于 init-declarator"写在一起。实现天然就是对的（clause 分支在 `eat_function_qualifiers` 读完 `-> T` 之后），现在有测试钉住"错的那半要报错"。
3. **比标准宽的那一份是刻意的**：`void f() requires true;` 是**非模板函数**，按 [dcl.decl.general]/5 也错，但它被读——函数声明符后面的 clause 只能跟函数体或 `;`，没有第二种读法，宽容不会产生错树。这条写成了带理由的测试，不是默认行为。

**约束的语法比表达式窄，这一点也查过**：`requires-clause: requires constraint-logical-or-expression`，而 constraint-logical-or-expression 是**由 `&&`/`||` 连接的 primary-expression**，所以
```cpp
template <int N> requires N == sizeof new unsigned short int f();   // 标准注释：error: parentheses required
template <int N> requires (N == 0) void f();                        // 对
```
标准注释把这条讲得很直白：**不是 primary expression 连 `&&`/`||` 的东西必须加括号**。parser 读的是**完整表达式**（比标准宽），因为约束后面紧跟着声明，宽容换来的是"少一个假报错"，而不是错树：`requires N == 0 void f();` 里 `void` 接不上表达式，clause 自然在那里停住。④（原子约束的括号形式）因此是**读得到的**，`ParenExpr` 保留括号。

**这一条里另一个顺带发现**：模板头的 `enter_template_arguments` **一直开到了 clause 里**。参数列表结束时角括号已经闭合，但深度没有还原，于是 `template <typename T> requires (sizeof(T) > 1) void f();` 里的 `>` 被当成"闭合一个早已结束的模板实参表"，报 `expected )`。修复是在参数列表之后把深度还原成**进入本模板头之前**的值（不是 0——模板头本身可能嵌在模板实参里）。

三个判断值得单独记：

1. **clause 的判据是"试读一遍，看它是否消费了 token"**，不是"下一个 token 在不在某张表里"。第一版用了后者，列了一张"能开始表达式的 token"表，**几分钟内就错了**：漏了 `&&`，于是 `requires C<T> && C2<T>` 把 clause 读成到 `C<T>` 为止。试读没有这张表要维护，用的是这个 parser 早就在别处用的有界回溯。
2. **约束里拒绝 `{` 作为花括号初始化**（`CppParser::constraint_depth`）。clause 夹在声明符和函数体之间，`requires C<T> { }` 里的 `{` 是**函数体**；而表达式语法把表达式后面的 `{` 读成 C++11 的临时量列表初始化（`Vec<int>{1,2}`）。不拒绝就会得到"C<T>{} 的约束 + 没有函数体"——良构、无损、无报错。深度而不是 bool，因为约束会嵌套（requires 表达式里的 requirement 又是表达式）。
3. **`noexcept` 进了一元运算符，但带条件**：只有后面跟着 `(` 才算（`noexcept(g())` 是表达式），否则那是函数的异常说明。它在清单里从来没被当过运算符，所以 `requires requires(T t) { { t.f() } noexcept -> int; }` 之前停在 `noexcept` 上。

**另外三件旧缺陷，被 C1 露出来并一起修掉**（都不是 C1 自己的规则；加上前面的类头 A0 与模板头角深度，这一条一共带出五件）：

- **模板实参表的收尾判据方向错了**（`closer_belongs_to_this_list`）。它从 `>` 往后扫，一看到 `<` 就认为"这个 `>` 是别人要的"。方向反了：**右边的 `<` 说明不了左边的 `>`**。后果是 `C<T> && C2<T>` 在第一个 `>` 上失败——而这不是罕见形状，它是**每一条两个 concept 的合取**，`T<A>::value < T<B>::value` 是同一个形状。现在元素边界上的 `>` 直接就是本表的收尾符：能回到这个边界，说明前面那个实参里所有嵌套的表都已经收完了。
- **声明符的名字不能是裸 template-id**（A0 级，静默错树）。`C<T> && C2<T>;` 在修好上一条之后仍然"通过"——被读成**声明**：`C<T> &&` 是右值引用类型，声明符名叫 `C2<T>`。良构、无损、无报错、无 `ErrorNode`，而这是任何编译器都不会接受的绑定。判据在 token 里就有：`C<T> x;` 的实参属于**类型**，名字是 `x`；template-id 只有当它是**限定名**时才是名字（`S<T>::f`，实参属于限定部分）。例外只有一个——**显式实例化**，那里名字真的是 template-id（`extern template void f<int>(int);`），所以标志位只在那条路上置起，且由 `parse_declaration` 在每条路径上复位。
- **花括号初始化列表在表达式位置没有读法**：`x = {1};`、`x += {1};`、`v.push_back({1, 2})`、`f({1})`。C++ 在这里要的是 *initializer-clause*，比表达式宽——赋值右操作数和调用实参都是。`x = {1};` 曾经靠 A0-1 的错树"过"，A0-1 修好之后它变成响亮报错，缺的规则才露出来。**一个缺陷会遮住另一个缺陷**：这不是新坏的，是一直错着、只是错得安静。

**护栏**：`tests/concepts.rs`（12 条：四种子规则、两个标准位置、一个宽容位置、一处**拒收**、括号形式与角括号记号、clause 范围、body 不被吞、类头拒收后类体仍在类里）、`expressions.rs` 的两条、`gaps.rs` 已支持清单里的 16 条与形状断言 4 条。

**一句话结论**：C1 的代价不在"写四条规则"，而在**它把语句读成表达式之后，原先被错树遮住的规则开始被走到**。这一条里三件旧缺陷都是这么露出来的，没有一件是 C1 弄坏的。

**仍不支持**（新登记，见下面 B13）：不带 `extern` 的显式实例化 `template void f<int>(int);`。（B12 的 `requires`/`concept` 作标识符已经修好，见该条。）

**分析层待办**（parser 之外的下一环）：`ConceptDecl` 是新节点，`scopes.rs` 的 `declaration` 走 `declaration_parts`，而后者在**没有声明符**的声明上早退（`is_unnamed_declaration`），所以 concept 的名字目前**不被绑定**——引用它解析不到符号。`BindingKind` 也没有 concept 这一种。修它需要新增一种绑定类别并想清楚 `is_type_like` 的答案（concept 不是类型，但出现在类型名的位置），所以单列，不塞进这一条。

### C2. 包展开（pack expansion）—— 已修复

```cpp
g(args...);                                            // 实参列表
std::tuple<Ts...>                                      // 模板实参
(ts + ...)                                             // 折叠表达式
sizeof...(Ts)                                          // 需要词法器配合
std::make_unique<T>(args...)
[args...] { ... }                                      // lambda 捕获
```

**性质**：缺规则。**一条规则覆盖十几处**——"模式后面跟 `...` 就展开"。

**声明侧从来不是缺口**：`Ts... ts`、`Ts&&... args`、`typename... Ts`、`class D : Bases...` 一直都是好的。

**修复**（`exprs.rs`、`types.rs`）：四处改动
1. `parse_expr` 末尾：表达式后跟 `...` 就包成 `PackExpansionExpr`。一个位置覆盖实参表、模板实参、初始化列表、`h(f(x)...)`。
2. `sizeof...`：词法器产出 `sizeof` + `...` 两个 token，`parse_unary_expr` 里把这一对连起来读。
3. 模板实参：type-id 读到 `...` 处会停（`...` 不是类型能接的东西），在那里把省略号当作本实参的一部分消费掉——否则实参列表的循环会回过头把 `...` 当成一个实参，报"读不出实参"。
4. 折叠表达式：`...` 读成 primary expression（新节点 `FoldExpr`），于是普通二元规则自然产出 `BinaryExpr(ts, +, FoldExpr(...))` 和 `BinaryExpr(FoldExpr(...), +, ts)`——**两种拼法共用一套代码**。

**期间发现并修掉的两件事**（都是"省略号被抢"）：
- `case 2 ... 4:`（GNU 区间）坏了：case 规则读一个表达式**然后**找区间的 `...`，而新的展开读法把省略号先吃掉了。加了 `parse_expr_without_pack_expansion` 给它用。
- `[[` 与 `[` 无关但同类：见 B3。

**教训**：把一个"到处都能出现"的读法放在公共入口上，必须同时把所有**自己拼这串 token** 的规则列出来。当时的清单漏了 GNU case 区间和 lambda 捕获列表，两个都是靠语料库和抽查发现的——`cargo test` 全绿的时候它们已经坏了。


---

## D 类：非标准扩展（建议搁置）

| 构造 | 说明 |
|---|---|
| `asm volatile("nop" : : );` | GCC 的 asm 限定符 + 冒号形式。裸 `asm("nop");` 反而能过（被当成普通调用） |
| `int x asm("eax");` | 同上 |
| `__attribute__((packed))` | 只在函数体外的位置碰巧能过 |
| `__declspec(...)` | 未测，同属 MSVC 家族 |

这些不是 C++，加了规则等于给方言做语法。**长期搁置**。

---

## 刻意的取舍（不是待办，不要"修"）

> **注意**：标了"取舍"不等于判断一定对。T1 就是一个被证明**不需要**跨文件信息的取舍——见下。判断一条取舍是否成立，要问的是"它真的需要文件外的信息吗"，而不是"它看起来难不难"。

### T1. C 风格转换（cast）—— 已修复

```cpp
auto d = (int)1.5;       // 已修复（关键字类型，一直能读）
auto d = (T*)p;          // 已修复 —— 曾被判为"取舍"，那个判断是错的
auto d = (MyType*)p;     // 已修复 —— 文件里根本没声明 MyType 也能读
auto d = (MyType)1.5;    // 已修复 —— 曾被称为"真正的取舍"，这个判断**也是错的**
auto d = (MyType)x;      // 已修复
auto d = (size_t)size;   // 已修复 —— 真实 C 文件里到处都是这个形状
```

**先后两次把这半条判成取舍，两次都错了**，而且错法不同——这是本文档里唯一一条被推翻两回的判断，值得完整记下来。

**第一次**（指针形式）：理由是"`*` 既是指针声明符又是乘号，`(a*b)` 和 `(a* b)` 的区分需要跨翻译单元的类型索引"。

1. `(a * b)` 与 `(MyType*)p` **不是同一个形状**——前者的 `*` 两侧都有操作数，后者的 `*` 左边什么都没有；
2. 更关键的是，**紧跟 `)` 的 `*` 根本不可能是二元运算符**，因为二元运算符必须有右操作数。

修复：`is_a_type_in_parentheses` 增加第三种确定形状——括号内容以 `*`、`&`、`&&`（含 `* const` 这类带 cv 的写法）结尾。

**第二次**（裸名字形式），理由是"`(MyType)` 既是合法括号表达式又是合法 type-id，只有名字查找能分辨"。**这句话只对了一半**：`(MyType)` 本身确实两可，但**它后面那个 token 把话说完了**——两个操作数并排不是任何文法里的表达式，所以

```text
(size_t)size     `)` 后面是标识符 —— 只能是 cast
(MyType)1.5      `)` 后面是字面量 —— 只能是 cast
(MyType)new T    `)` 后面是只能作前缀的关键字 —— 只能是 cast
```

**第五种确定形状**（`exprs.rs::an_operand_follows_the_parentheses`）：扫到配对的那个 `)`，看它后面那个 token 是不是"只能开启一个操作数"的。判据是**推导出来的，不是猜的**：这些输入在表达式文法里**根本不成句**，所以 cast 读法不拿走任何能读的东西。

**刻意留在集合外的 token，各有各的理由**：

| token | 为什么留下 | 例子 |
|---|---|---|
| `(` | `(f)(x)` 是**调用**，读成 cast 会丢掉被调用者和实参 | `(a)(b)` |
| `*` `&` `+` `-` `++` `--` | 每一个**同时是二元运算符** | `(a) - b` 是减法，`(a) * b` 是乘法 |
| `[` | `(a)[b]` 是下标 | `(a)[b]` |
| `,` | `(a), b` 是逗号表达式 | `(a), b` |

所以残留的"两可"缩到这些形状上：`(a)*p`、`(a)-b`、`(a)(b)`、`(a)[b]`。它们**全都是合法表达式**，与上面那些"根本不成句"的形状不是一类——这也正是这条判据能成立的原因，`operators.rs` 两侧都钉住了。

**为什么两阶段是必要的**：`(a)` 能解析成 type-id（一个名字、没有声明符），所以"能试就试类型读法"会把每个括号变量变成 `a` 的 cast。第一阶段只是廉价的"值不值得试"，第二阶段（试 cast、失败就回退成括号表达式）才是判据——**这也是为什么第一阶段不必精确**。

**发现经过**：第二半是拿 `cpp_dump` 跑一个真实的 C 文件（LuaJIT 宿主 `main.c`）撞出来的——13 个报错里有 4 个是 `(size_t)size`，而且它出现的位置（`malloc((size_t)size + 1)`）是教科书级的 C 写法。**这次不是靠"看起来难不难"判断，是靠真实文件说话。**

**方法论修正**：取舍条目当初是按"看起来难不难"判断的，不是按"它真的需要文件外的信息吗"。这条判断被推翻两次，两次都是同一个错误：**先假定"两可"，再假定"两可就必须查表"**——而两次的答案都藏在"两可"之外的那个 token 里（`*` 的左边、`)` 的右边）。已在"刻意的取舍"章节开头加了提醒。

`gaps.rs` 里指针形式与裸名字形式**都已移入"能读"清单**并加了 `CastExpr` 形状断言；"不读"清单里这一条现在只剩一段说明——**空列表本身就是结论**。

### T2. `x = a < b > c;` 读成 template-id —— 已修复，且原先的判断是错的

原先记的是：`a_matching_angle_bracket_follows` 的浅扫描会被没有空格的 `a < b > c` 骗到，读成模板实参列表；取舍方向是"**失败模式是良性的**（token 还是被解析了，只是读成了 template-id 而非比较），不值得为它做完整表达式分析"。

**这句话错了两次**，B21 里记了完整的复盘：

1. **它不是良性的**。读成 template-id 之后，后面那个 `c` 无处可去，结果是**报错**（`expected ; after expression`），不是"另一种读法"。真实文件里 `if (n < 0 || n > 100000)` 直接挂在这上面。
2. **不需要"完整表达式分析"**。需要的只是看 `>` **后面那个 token**：操作数跟在 template-id 后面在 C++ 里不成句，所以那个位置有操作数就说明 `<` 是小于号。判据是推导出来的，与 T1 的"`)` 后面跟操作数"是同一条。

这一条值得和 T1 并列记住：**两次"取舍"的失败原因完全相同——先假定"两可"，再假定"两可就必须做重活"**。而两次的答案都在"两可"之外的某个 token 上（`*` 的左边、`)` 的右边、`>` 的右边）。判断一条取舍时该问的是"**有没有一个 token 能把它判死**"，不是"要不要写个更重的分析器"。

### T3. `operator T&&()` 与引用限定符共用 `&&`

```cpp
operator T&&()      // && 和 ( 贴在一起 -> 类型继续
operator bool() &&  // && 和 ( 分开     -> 这是成员函数的引用限定符
```

判据是**源码相邻性**（`peek_token_range_at`），和 `<=>` / `<` 那类问题用的是同一类证据。失败模式：有人把 `operator bool()&&` 写成没有空格，名字会多读一个 token、声明报缺 `;`——响亮、局部、改一个空格就好。

### T4. 跨编译单元的类型信息 —— 已给出设计：不在 parser 里补，而是开一个接口

`TypeNames` 表是**文件局部**的。头文件里的类型、模板参数、没见过的 builtin，它一概不知道。这不是 bug，是"不等符号表"的直接推论。表的定位见 `crates/cpp_parser/src/parser/type_names.rs` 的模块文档，用法见 `cpp_parser/src/grammar/cpp/types.rs` 的模块文档（"Everything in this module is *syntactic*. Name lookup is deliberately absent"）。

**现在有了正式答案：外部符号表**（`crates/cpp_parser/src/symbols.rs`；接口定义在 parser 侧，实现由 `cpp_code_analysis` 提供）。要点：

1. **两层**：有表就用表（它知道头文件与别的翻译单元），没有表、或者表也不知道，就回落到既有的形状偏好——**表是偏好，不是依赖**：`ParserConfig::default()` 的现有调用一行不改，行为与今天逐字节相同（`tests/symbols.rs::an_empty_table_parses_exactly_like_no_table` 用真实语料 `real_world.cpp` 把这条钉住了）。
2. **三值，不是布尔**：`kind_of(name) -> Option<SymbolKind>`，`None` 是"**这张表不知道**"，绝不是"不是类型"。"否"只能由 `Some(其它种类)` 表达——比如 `Function` 用来**否决**声明读法。把两者合成 `bool`，会让一个过期的索引悄悄改变合法代码的读法，那正是 A0 类的问题。
3. **优先级**：本文件（`TypeNames`/`MacroNames`——就是正在解析的文本，永远最新）→ 外部表 → 形状偏好。索引是**滞后**的，所以排在"文件自己说的话"之后：刚改名的符号由第 1 条或"不知道"回答，永远不会被过期的第 2 条抢先。
4. **词汇表是"判据驱动"的**：`Type` / `Template` / `Macro { function_like, body }` / `Function` / `Variable` / `Namespace`。其中最值钱的是 `MacroBody`（`Specifier` / `Statement` / `Block` / `Expression` / `Type` / `Unknown`）——它正好对上这几轮所有宏形状（`MY_API` 是 Specifier、`NUMBER_OPTION` 是 Statement、`IF_EXIST` 是 Statement + 块、gtest 的 `TEST` 是 Block），于是"按形状猜"可以变成"查表"。`Unknown` 必须存在：索引常常知道"这是宏"却不知道宏体。
5. **契约**：表只决定**读法**、不决定**结构**——喂一张胡说八道的表（"一切都是宏"、"一切都是类型"）树仍然无损、良构、不 panic，`tests/symbols.rs::a_hostile_table_cannot_break_the_tree` 就是这么测的；表是只读纯函数（`&self`、`Send + Sync`、无内部可变状态），所以**不进 `Checkpoint`**，回滚不需要恢复它；一次 parse 的结果是 `(文本, 表)` 的函数，索引更新后重新解析即可。

**接入点与进度**（都是这几轮收敛出来的具名判据，不需要新层）：

| 判据 | 表能给的证据 | 状态 |
|---|---|---|
| `at_a_macro_call_statement`（B41） | `Macro{body:Statement/Block/Unknown}` | **已接**：`may_be_a_statement_without_a_semicolon`——`Expression`/`Type` 体**故意答否**（`MAX(x,y)` 漏分号仍要报） |
| 类体成员位置的裸宏（`Q_OBJECT`、`Q_PROPERTY(...)`） | `Macro{body:Statement/Block/Unknown}` | **已接**：新增 `at_a_macro_member`/`parse_macro_member`，产出同一个 `MacroCall`；带 `(` 的形状要求组后没有 `;`，裸写法的判断用的是**表描述的宏体**（本地 `#define` 没有宏体可查，才退回形状保守判断） |
| `a_declaration_is_the_better_reading`（`Widget w(1,2)` vs `g(1,2)`） | `Type`/`Template` ⇒ 声明；`Function`/`Variable`/`Namespace` ⇒ 不是 | **已接**：这是这张表最主要的存在理由，也是本地表永远给不出的那个"否" |
| `is_a_type_in_parentheses`（T1 的 cast 判据） | `Type`/`Template` | **已接**：`(Widget)x` 对头文件里的 `Widget` 也能读成 cast |
| `a_macro_definition_follows`（B32/B36） | `Macro{body:Statement/Block/Unknown}` | **已接**：体内那个形状通常由**语句规则**先一步接走（`MacroCall` + 块），所以这次咨询多数时候是冗余的——接的理由是**一条规则不该依赖另一条规则的执行顺序**；拼写约定退成最后兜底（头文件宏、没人索引过的那种） |
| `name_joins_the_type`（`MY_API Widget *p;`） | `Type`/`Template`（在已经并进一个名字之后） | **已接**：常见写法（`MY_API Widget *p;`）本来就由 B29 覆盖，表补的是形状判据看不见的那半——`MY_API Widget const w;`：`const` 不是声明符能延续的东西，没有表时类型会停在 `const` 前面 |
| `a_bare_template_id_is_here` | `Template` | **判断为不需要**：它问的是"裸 template-id 不能当声明符名字"，是对**token 形状**的判断，与"这个名字是不是模板"不是同一个问题；硬接进去只会把两件事混在一起 |

落地顺序：**接口（已完成）** → **宏优先（已完成）** → **类型优先（已完成，除下面那条判断为不需要的）** → `cpp_code_analysis` 实现 trait（未开始）。每一步的证据都在 `tests/symbols.rs`（8 条）：表命中、表未知、**表喂错**（`Expression` 体、"表说是宏但名字不像宏"、`Specifier` 体在体内）三种情形各有用例，另有"空表 ≡ 无表"的逐节点等价断言。

---

## 实施顺序

按"静默错树的先修、一条规则覆盖多处的优先"排：

| # | 任务 | 级别 | 成本 | 状态 |
|---|---|---|---|---|
| 1 | 转换运算符（含限定名、引用限定符） | A1 | 半天 | **完成** |
| 2 | 包展开（含折叠表达式、`sizeof...`、捕获列表） | C2 | 一天 | **完成** |
| 3 | 别名 `using` 的数组/函数类型 + 属性位置 | B2, B3 | 一天 | **完成** |
| 4 | `alignas`（含成员位置） | A2, B1 | 1–2 天 | **完成** |
| 5 | 括号表达式 + 三元运算符 + lambda | B7 | 一天 | **完成** |
| 6 | 赋值被读成声明（含 `gaps.rs` 形状护栏） | **A0-1** | 半天 | **完成** |
| 7 | 逗号运算符（先写"自己拼 token"清单） | B4 | 半天 | **完成** |
| 8 | C 风格指针转换 `(T*)p` | T1 缺规则的那半 | 半天 | **完成** |
| 9 | 显式对象参数（含"普查发现静默错树"） | A3 | 半天 | **完成** |
| 10 | 替代运算符记号 + `throw` 表达式 | B8, B9 | 半天 | **完成** |
| 11 | `extern template` + `inline namespace` | B10 | 半天 | **完成** |
| 12 | `decltype` 作类型说明符（两个叠加缺陷） | B11 | 半天 | **完成** |
| 13 | concept / requires（含三件顺带发现） | C1 | 1–2 周 | **完成** |
| 14 | `requires`/`concept` 作标识符（词法层拆关键字） | B12 | 半天 | **完成** |
| 15 | 不带 `extern` 的显式实例化（含显式特化回归） | B13 | 半天 | **完成** |
| 16 | 真实 C 文件暴露的四件：裸名字 cast、相邻字符串字面量、`for` 步进、用户自定义字面量 | T1, B16, B17, B18 | 半天 | **完成** |
| 17 | 同项目第二个文件暴露的：`sizeof` 操作数只要不是裸名字就失败 | B19 | 半天 | **完成** |
| 18 | `<` 是模板实参表还是比较：试读 + 用"后面那个 token"验证 | B21 | 半天 | **完成** |
| 19 | clause 内的括号深度判据（含 cast 判据的同族误伤） | B22 | 半天 | **完成** |
| 20 | 限定名那一族：定义头部的形参列表（含前导存储说明符） | B14, B15 | 一天 | **完成** |
| 21 | elaborated type specifier 进类型读法（`sizeof(struct S)`、`(struct S*)p`） | B20 的一半 | 半天 | **完成** |
| 22 | 类型后的 cv 限定符（两个叠加缺陷；修完 CMake 文件 10→4 条） | **A0-4** | 半天 | **完成** |
| 23 | 数组类型进类型读法（`sizeof(int[4])`、`Vec<int[4]>`；含 `new` 的开关与方括号配对的扫描） | B20 的另一半 | 半天 | **完成** |
| 24 | 构造中间的预处理条件行（初始化列表元素之间 + 字符串字面量串中间） | B23 | 一天 | **完成** |
| 25 | K&R 风格函数定义（两个叠加缺陷：括号外形参表 + 那个不存在的 `;`；修完真实项目全绿） | B24 | 半天 | **完成** |
| 26 | 枚举量的初始化式吃掉后续枚举量（**A0 静默错树**；真实 C++ 项目探针的头号发现） | **B26** | 半天 | **完成** |
| 27 | UTF-8 BOM 当空白（37/200 个真实文件，一个字符覆盖最大面） | B27 | 一小时 | **完成** |
| 28 | 直接初始化判据只看到一层（限定名调用里的嵌套实参） | B28 | 半天 | **完成** |
| 29 | 声明开头的"宏 + 类型名"两个裸名字（`MY_API SomeClass *f();`；第一版误伤 requires-clause，被形状断言抓住） | B29 | 一天 | **完成** |
| 30 | 嵌套 `>>` 与空模板实参 `std::less<>`（三处数角度的扫描，抽出 `angle_depth_delta`） | B30 | 半天 | **完成** |
| 31 | 成员指针：`->*` 与 `(Class::*name)`（含 `int C::*p;` 的**静默**错树：声明符钻进类型节点） | B31 | 一天 | **完成** |
| 32 | gtest 风格的"宏 + 块"（6 个测试文件，最多 98 条级联；三条例外由既有测试逼出） | B32 | 半天 | **完成** |
| 33 | 字面量后缀不必以 `_` 开头（`1k_row`、`100ms`、`"name"sv`；真因在词法层） | B33 | 一小时 | **完成** |
| 34 | 表达式里以 `::` 开头的限定名（删掉 `is_a_type_in_parentheses` 里那条凭"看起来像类型"的 arm） | B34 | 一小时 | **完成** |
| 35 | 函数式转换 `bool(x)`（类型只吃关键字本身，不能交给 `parse_type_id`） | B35 | 一小时 | **完成** |
| — | 函数体内的"宏 + 块"（`IF_EXIST(k) { … }`；按宏书写约定区分于漏分号） | B36 | 半天 | **完成** |
| — | 类体后面的声明符名字（`struct S { … } x;`；修前是**静默**错树） | B37 | 半天 | **完成** |
| — | 函数类型当模板实参（`A<bool(T)>`；数尖括号的预查把配对的 `)` 当边界） | B38 | 半天 | **完成** |
| — | 成员函数体里的范围 `for`（位域判据改用大括号栈问"最里层是不是类体"） | B39 | 半天 | **完成** |
| — | 指令落在 `try` 的两个接缝上 | B40 | 半天 | **完成** |
| — | 宏调用省略分号（宏体自带语句）——**修法是建 `MacroNames` 宏表**，拿 `#define` 当证据 | B41 | 半天 | **完成** |
| — | `::new` 的全局限定（`::` 被名字分支吃掉，整个分配读成一个**扁平**的名字节点） | — | 一小时 | **完成** |
| — | `__try` / `__catch`（标准库自己的 `bits/exception_defines.h` 把 `__try` 定义成 `try`） | — | 半天 | **完成** |
| — | `throw()` 动态异常规格（C++17 删除、标准库仍写 63 处；`new_handler …(…) throw();` 还要靠它断案） | — | 半天 | **完成** |
| — | `if _GLIBCXX17_CONSTEXPR (…)`（`if` 与条件之间的宏）与子语句上的 `[[likely]]` | — | 一小时 | **完成** |
| — | 编译器自己的关键字（`__cdecl` 等 1464 处）——**静默错树**，声明符的名字成了 `__cdecl` | **A0** | 半天 | **完成** |
| — | 函数 try 块 `void f() try { } catch (...) { }` | B25 | 半天 | 待办 |
| — | `namespace` 与名字之间的属性 | B3 残留 | 半天 | 待办 |
| — | `void()` 作表达式 | B5 | 半天 | 待办（很少见） |
| — | `#if` 落在 `if` 与它的 `else` 之间 | B23 的第三个接缝 | 半天 | 待办 |
| — | 变量模板的偏特化（`template <class T> constexpr bool v<T*> = true;`） | — | 半天 | 待办 |
| — | 模板参数表里的宏（`typename... _ArgTypes _GLIBCXX_NOEXCEPT_PARM`） | — | 半天 | 待办 |
| — | `typedef __typeof__(…)` 与 `__int128`（GNU 扩展，6 + 10 个文件） | — | 半天 | 待办 |
| — | 小写函数式宏独占一行（`__glibcxx_function_requires(…)`） | — | 半天 | 待办 |
| — | `asm volatile`、`__attribute__` | D | — | **不做** |

第 16 项是一次**用真实文件做探针**的结果，方法和前面 15 项都不同，所以单独记一笔：把 `cpp_dump` 指向一个真的 C 程序（LuaJIT 宿主 `main.c`，120 行），13 个报错按"同一个成因"归类后只有 **3 个根因**，加上写测试时撞出的第 4 个，四条各半天不到。**如果没有真实文件，这四条一条也不会被发现**——`gaps.rs` 的普查清单是按"能想到的构造"列的，而这四条都是"想不到但遍地都是"的类型（`(size_t)size`、十个相邻字符串字面量、`for (;; i++)`、`1_km`）。这也解释了为什么第 16 项里有三条来自同一个文件：**一个真实文件的覆盖面，比一份手写清单更宽**。

第 6 项排在 C1 之前，理由和它的级别一样：它是**唯一一类连 `ErrorNode` 都不留的错树**，而 C1 虽然贵，至少是响亮的。

第 13 项的实际成本远低于预估的"1–2 周"，原因值得记下来：**四件事共用一条判据**（试读一遍看是否消费 token），而不是四条各自维护一张 token 表。真正花时间的是它顺带暴露的旧事——模板实参表的收尾方向、声明符名字不能是裸 template-id、花括号初始化列表在表达式位置没有读法、类头 clause 把类体丢掉、模板头角深度漏进 clause、无名形参被读成变量——**没有一件在 C1 的计划里，也没有一件是 C1 弄坏的**。它们是 C1 把语句读成表达式之后才**露出来**的：一个缺陷会遮住另一个缺陷。

第 7、8 项连着做，因为是同一件事的两面：两个运算符都是"同时也是标点的运算符"，都需要一条**不在运算表里**的判据。第 7 项的清单（`exprs.rs` 的 `Level` 文档）在第 8 项里没有用上——cast 不与任何列表争 token——但它在第 7 项自己身上抓到了两个漏掉的调用点（pack expansion 与位域宽度），值得保留成模板。

## 一个反复出现的教训

第 1、2、4 项都撞上了同一件事，值得单独记下来：**改一个公共入口的读法，会同时改掉所有"自己拼这串 token"的规则，而 `cargo test` 全绿不代表没坏。**

- 第 2 项（让 `...` 在 `parse_expr` 里多一种含义）弄坏了 GNU case 区间和 lambda 捕获列表——两个都是语料库和抽查发现的。
- 第 4 项（把「见过 specifier」拆成「见过类型」）弄坏了 `std::vector<int> values;`（specifier 以 `>` 结束）和 `friend` 之后的成员（friend 的载荷就是后面整个声明）。

两个都是**同一个标志的两种边界**，而且都不是 `alignas` 测试能覆盖的。所以维护约定第 5 条不是形式主义：改这类规则时，先把"哪些地方自己拼这串 token"列出来，逐个验证。

**第 6 项给出了另一半答案，而且更根本。** 上面两次靠的是"语料库和抽查"——那是**运气**，不是机制。A0-1 说明"报错/无损/良构"这三种判据合起来仍有盲区，因为**一棵错的树同样可以无损、良构、无报错**。所以护栏要问的不只是"干净吗"，还有"读成了什么"：`gaps.rs::constructs_are_read_as_the_right_node` 就是这个问题的实体。发现 A0-1 靠的是一次无关的探针，而它当时已经存在于**几乎每一个函数体里的每一条赋值**。

## 标准库那一批（第八轮）：宏站在关键字位置、编译器自己的关键字、两种读法都成功时谁断案

这一轮是照 [`std-library.md`](std-library.md) 的 P1 队列做的，一次做完六条，闭包 **88 → 97 个文件干净**（185 个文件）。按"证据先于规则"记下四条，因为它们的形状会重复出现：

**一、`::new` —— 关键字跟在 `::` 后面。** `::new (p) T(args)` 是"要全局的 `operator new`"，41 处 / 13 个文件（`std::construct_at`、各分配器、`std::exception_ptr`、`std::pmr`）。`::` 本来是**限定名**的开头，于是名字分支把它吃掉，在 `new` 上报 `expected a name after ::`，恢复又把整条分配读成**一个扁平的名字节点**——不是读错节点，是**没有结构**。修法是在一元表达式分派里加一条 `Scope if peek == NewKeyword`，`::` 归 `NewExpr` 所有（两个拼写调用的是同名的不同重载函数，丢掉限定就丢了语义）。

**二、`__try` / `__catch` —— 实现自己的拼写。** `bits/exception_defines.h` 里写着 `#define __try try` / `#define __catch(X) catch(X)`（关掉异常的那一支是 `if (true)`/`if (false)`）。它们**不是本文件的宏**，所以"从表里拿证据"这条路走不通；而按形状读成"宏 + 块"会丢掉 try 与 handler 的配对关系。按第 24 条（`__attribute__`/`__declspec`）的两个条件——名字由标准保留给实现、含义来自实现自己的头文件——按拼写读成 `TryStat`/`CatchStat` 是**有证据的**，不是约定。修前 `__try { g(); }` 的读法是 `ExpressionStat(InitListExpr(__try, InitListExpr({ g(); })))`：名字后面跟 `{` 是"花括号初始化临时量"，于是块里那条语句报 `expected }, but get ;`，接着**整个函数的括号配对差一位**（`expected }` + 一个游离 `ErrorNode`）。18 个文件里有它。

**三、`throw()` 动态异常规格 —— 后缀位置上的关键字。** C++17 删掉了它，标准库还写着 63 处。它在**声明符后缀**位置上和 `noexcept` 同一格，但载荷是**类型表**而不是表达式，所以读法是 `parse_type_id` 逐个类型 + 逗号。它顺手解决了另一件事：`new_handler set_new_handler(new_handler) throw();`（`<new>`）里两条读法都成功——`(new_handler)` 既是一个无名形参表，也是一个带括号的值——是**后缀**断的案：变量声明不可能有 `throw(…)`，所以那对括号是形参表。这条判据（`a_dynamic_exception_specification_follows`）写在两处"优先读初始化式"的门口，一处实现。

**四、编译器自己的关键字 —— 一个纯粹的静默错树。** `int __cdecl g(void);` **解析成功**，读成"类型 `int`、声明符名叫 `__cdecl`、后缀是一个宏调用 `g(void)`"——这三个位置（类型、名字、宏后缀）都是本语法**故意**支持的读法，所以无损、良构、零诊断，而里面每一个名字都是错的。这就是 A0 那一类，也正是 `gaps.rs` 的形状断言能抓住的东西：断言"这里必须有一个 `ParameterList`"，错读法一个也产不出来。实测 `__cdecl` **1464 处 / 19 个文件**、`__restrict` 365 / 8、`__extension__` 61 / 12、`__forceinline` 3 / 2，`__int64`（`_mingw.h` 里是 `#define __int64 long long`，所以它是**宏**，读法本来就对）81 / 14。两个位置都要接：说明符序列里（`int __cdecl g(void)`）和声明符的限定符位置（`int *__cdecl _errno(void)`、`const char * __restrict__ _Src`）。`__restrict` 顺便兑现了 kind 表里一直没人产出的 `RestrictQual`。

**顺带一个反直觉的观测**：这一条把 19 个文件里 1464 处的错树改对了，而普查只多了 **1 个干净文件**——因为那些文件本来就"解析成功"。**普查看得见报错，看不见错树**；判断一轮的收益时，两个数字都要看。

**五、`if _GLIBCXX17_CONSTEXPR (…)` 与 `[[likely]]`。** 前者是宏站在 `if` 与它的条件之间（C++17 展开成 `constexpr`、C++14 展开成空），按形状接受它**不花任何合法程序**：`if` 后面必然是 `(`。后者是子语句上的属性（`if (x) [[likely]] y;`），16 处 / 7 个文件，全部在这一个位置；读它的地方放在 `parse_statement_body` 里，于是 then 分支、else 分支、每个循环体一次覆盖（属性节点是语句的兄弟，不是体的一部分）。

**六、这一轮最值得记的一条教训**（已加成维护约定第 28 条）：`parse_one_decl_specifier` 判断"这个说明符有没有命名类型"靠的是**它消费的最后一个 token 是不是标识符**，所以把一个**以标识符结尾**的新说明符塞进那条分支，会静默把 `has_type_specifier` 置真。症状是 `__forceinline size_t f() { }` 报错而 `__forceinline int f() { }` 正常——`size_t` 被当成声明符的名字了。修法不是改判据，而是把这类关键字**挪到那个函数外面**消费。

## 标准库那一批（第九轮）：三处"包着构造的东西"和一处"作用域"

这一轮同样是照 [`std-library.md`](std-library.md) 的 P1 队列做的，闭包 **97 → 101 个文件干净**（185 个），而**消息总数从约 2423 降到约 2252**——两个数字都要看，因为这一轮的四条里只有一条直接把文件推到干净，另外三条是把文件里"第一个错"往后推。四条有一个共同点：**判据问的都不是"这个 token 是什么"，而是"它被什么包着"**。

**一、链接规范的块不是 body（一次修掉 4 个文件）。** `extern "C++" { namespace std { … } }` 是几乎所有 libstdc++ 头文件的骨架，而 `parse_linkage_block` 会 `enter_type_name_scope()`——理由是"链接块是一个作用域，里面声明的类型外面不是"。这个理由**与它自己上方的文档矛盾**（那段写着"里面的名字之后仍可见"，标准也如此：链接块只影响 language linkage，不引入名字作用域），而且它还有一个更远的后果：`is_inside_a_body()` 读的正是这个深度，于是 parser 认为自己在一个 **body** 里，而"从头部来的宏"那两条规则（`at_a_macro_that_stands_for_a_declaration`、`a_macro_definition_follows`）**按设计拒绝在 body 里生效**。结果是 `_GLIBCXX_BEGIN_NAMESPACE_VERSION` 被读成一个普通名字，它后面那条声明被读成表达式：`cwchar` 的第一个错是 ``expected `;` after expression`` 指着 `using ::wint_t;`，`cstdlib` 同理指着 `using ::div_t;`。删掉那一对 enter/leave 即可，因为**一个计数器被两个问题共用**：`is_at_file_scope` 问"我和文件之间有没有作用域"，`is_inside_a_body` 问"我在不在一个 body 里"，而链接块是前者、不是后者。

**二、变量的偏特化：`template <class T> bool v<T*> = true;`。** 阻止"裸 template-id 当声明符名字"的规则（见第 13/21 条那一族）在这里必须让路——偏特化**必须**说出它对哪个模板特化。挂起那条规则的标志当时只在 `template <>`（空头）时设置，因为"显式特化"是当时想到的那个用例；而非空头引入的**偏特化**把参数写在名字位置上，没有别处可写。修法是把标志对**每一个**模板头都设上：它只在声明符的名字位置被查询，而头能引入的其它声明在那里不会写 template-id（`template <class T> C<T> x;` 的参数属于类型，早已被说明符序列取走）。代价：`bits/stl_pair.h`（`__is_tuple_v<tuple<_Ts...>>`）、`concepts`（`__destructible_impl<_Tp>`）、`bits/functional_hash.h` 各一个首错。

**三、模板头后面的无名数组参数：`template <class T> void f(int[4]);`。** 这一条的形状值得记：**无名的**声明符只有在 `declarator_starts_with_a_type_keyword` 回答"是"时才打开后缀循环（有名的靠自己的名字打开），而这个判据**向回走**找"这条声明的第一个 token"，一路上把模板头也走过去了，于是收集到的是 `template <` 的那个 `<`——不是类型关键字，后缀循环不开，`[4]` 无处可挂，参数表失败，然后外层声明符报 ``expected a parameter list or an initializer``。于是 `void f(int[4]);` 能读、`void f(int n[4]);` 能读，唯独 `template <class T> void f(int[4]);` 不能读，而且整条声明连同文件其余部分一起丢掉（实测树是扁平的）。修法是让那个回走**跨过整个尖括号表**（表里的 token 不是这条声明的开头，`std::vector<int> x` 的开头是 `std::vector`）**并在 `template` 处停下**。两半缺一不可：只停不停跨，收上来的是 `<`；只跨不停，收上来的是头的第一个 token。

**四、模板参数名是类型：`S<_Tp[_Nm]>`。** 这是最常见的数组偏特化写法（`is_array<_Tp[_Size]>`、`rank<_Tp[_Size]>`……闭包里 **51 处**，而且**没有一处**是"下标当非类型实参"的那种读法）。`_Tp[_Nm]` 是类型实参，当且仅当 `_Tp` 是类型；而 `S<a[0]>` 的 token 一模一样，所以只能靠"这个名字是不是文件声明的类型"来答。模板参数**正是**文件在类型位置声明的名字，但 `TypeNames` 记不下它：那张表的深度数的是**花括号**，而模板参数的作用域是**它那条声明**——记进去就会让这个名字在文件剩下部分一直是类型，正是那张表的文档明确拒绝的方向（"录得太少是安全的那一侧"）。所以给它一张自己的表：**每个头把类型参数追加进去，由拥有它的那条声明在结束时截断**（`parse_declaration` 保存长度并截回，嵌套声明各自的保存值里已经含外层的参数，所以类模板的成员照常看得见 `T`）。

## 标准库那一批（第十轮）：一条接缝的九种形状，和一个类的公开接口

这一轮的目标是**一个类的公开接口**。`declarations_in("std::basic_string")` 只有 **117 条**成员，全是 typedef 与私有
辅助函数（`_M_*`、`_S_*`），公开接口一条也没有；`examples/std_query.rs` 的 7 条查询是 **0/7**。也就是说：别名跟一步、
"文件自身守卫不算条件"、成员查找容忍重载这三处语义修复**都做完了，却没有东西可查**——挡在前面的是 parser。

成因自始至终是同一个：**一条规则在等一个特定 token，文件在那里写了 `#`**（或者写了另一种"只可能属于类型"的 token）。
这一轮把这条接缝在 `bits/basic_string.h` 里的九处一次做完。九处的后果都不是"报错"，而是**形状错**：出错的那条声明
把后面**所有成员**吞成自己的子孙——树仍然无损、良构，多数时候**零诊断**，但成员不再是成员。

| # | 形状（`bits/basic_string.h` 的行号） | 现在读在哪里 |
|---|---|---|
| 1 | `if` 分支与 `else` 之间的指令（490） | `parse_if_statement`（上一轮） |
| 2 | 类体里两个成员之间、access specifier 前后 | `parse_class_body_members`（上一轮） |
| 3 | 约束构造函数：clause 之后是 `#endif`，再是 `: _M_dataplus(…)`（585） | `finish_init_declarator` 的 requires 分支 |
| 4 | 模板头之后的 `#endif`（700） | `parse_declaration` 的模板头循环 |
| 5 | 一个声明**两个头**，`#else` 夹在中间（845） | 同上（头循环改成循环） |
| 6 | 两个头之间**还夹着说明符**（1673） | `parse_decl_specifier_seq_with`：指令之后可以再读一个头 |
| 7 | 属性之后、说明符之前（1310） | `parse_decl_specifier_seq_with`：说明符之间接受指令 |
| 8 | 声明符的 `&` 与它的名字之间（2631） | `parse_abstract_declarator` 的前缀循环 |
| 9 | 两个宏说明符 + 关键字类型（1329）：`_GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR` / `bool` / `empty()` | `a_declarator_still_follows_the_name`：跟随者是类型关键字 |

第 9 条不是指令，而是**同一个问题的另一面**：规则在等 token，来的不是 `#`，而是"另一种只可能属于类型的东西"。
同形的一共三处，另两处不在 `basic_string.h`：

- **`operator` 是声明符的开始**（1025）：`_GLIBCXX20_CONSTEXPR` / `_If_sv<_Tp, basic_string&>` / `operator=(…)`。
  改前这条成员是一个名叫 `_If_sv` 的**变量**——"模板 id 可以当名字"那条为变量模板偏特化写的例外吃掉了它，
  于是真正的声明符、形参表和函数体都无处可去，后面所有成员跟着一起丢。
- **析构函数名前面的宏**（`bits/stl_vector.h:372`）：`_GLIBCXX20_CONSTEXPR` / `~_Vector_base() _GLIBCXX_NOEXCEPT`。
  `~` 那一支的判据原来是"手里已经有一个类型名"，而**未展开的宏也是名字**，于是析构函数名被当成类型的又一个名字、
  `()` 成了 `ErrorNode`，`_Vector_base` 从那里塌到文件末尾。判据换成"刚消费的 token 是 `::`"——那才是
  "限定名里的析构函数"（`Foo::~Foo`）与"类里的析构函数声明"的区别，也正好是语法说的那件事。

**量到的**（同一台机器、同一条命令，都能重跑）：

```text
declarations_in("std::basic_string")     117 → 442 条（函数 103 → 391；size/length/find/substr/begin/end/… 全在里面）
basic_string.h 该作用域事实覆盖的行        588 → 3759（到类的收尾；类体是 93–3764）
std_query（examples/std_query.rs）        0/7 → 5/7（s.size、s.substr、s.empty 答在 bits/basic_string.h；
                                          v.push_back 答在 bits/stl_vector.h；m.begin 答在 bits/stl_map.h）
bits/stl_vector.h 的事实行数               37 → 469（`std::vector` 自己 0 → 113 条）
普查（128 个文件的闭包）                  干净 76 → 79 / 报错 52 → 49；消息总数 1435 → 1421
std_index                                 声明 4821 → 10378；类型 2770 → 5326（其中别名 305 → 709）
```

形状断言是 `gaps.rs::a_directive_inside_a_declaration_keeps_the_members_after_it_members`（九段最小复现，
每条断言的是"这个类**还剩几个直接成员**"，不是"解析成功"）。

### 这一轮的后半段：接缝修好之后，**恢复**才是决定性的那一层

九处接缝修完是 **3/7**，而 `std::vector` 依然一条事实都没有。同一套二分（把真实文件逐段外移 + 量"canary 成员
挂在第几层"）把剩下的三个成因挖了出来——三个都不是接缝，而是**"读坏了之后怎么办"**：

**一、失败的声明留下的节点必须带上结束事件。** `parse_declaration` 失败时，如果**已经吃掉了 token**，走的是
"保留 token、就地放弃"那条路（`close_marks_above(base)`）。那条路把节点**摘掉而不发 `NodeEnd`**——而一个没有
配对的 `NodeStart` 会被建树器在**流的末尾**补上结束，也就是说：**这条放弃掉的声明吞掉了写在它后面的所有 token**。

```cpp
struct Base {
  int x = 1        // 少一个 `;`，声明读到这儿才放弃
  int after;       // ← 于是这一条成了它的子孙：还在树里，但不再是成员
};
```

`bits/stl_vector.h:192` 那一条成员就是这样让 `std::vector` **一个成员都没有**的（`v.size` 与 `v.push_back`
都答"未声明"，类体一直拖到文件末尾）。修法是 `MarkerEventContainer::end_marks_to(target)`：**按身份、逆序、
带上 `NodeEnd`** 地把这次尝试开的节点关掉。它与 `finish_marks_to` 的分工是明确的——那一个是"这些 token 我不要了"
（调用方随后 `rollback`），这一个是"token 留着，但把它们关在该关的地方"。这是维护约定第 33 条。

**二、一个声明符只有一个初始化式。** `bits/stl_vector.h:192`：

```cpp
struct _Grow {
  _GLIBCXX20_CONSTEXPR void _M_grew(size_type) { }
};
```

宏站在返回类型的位置，而这个文件不知道它的含义。后缀读取器于是要回答 `(size_type)` 是形参表还是"给变量
`_M_grew` 的直接初始化"——它按"类型位置已经有一个名字"选了后者，把 `void` 是关键字类型这件事放了过去：成员
成了 `_GLIBCXX20_CONSTEXPR void` 类型的**变量** `_M_grew`，用 `size_type` 初始化，**再**用函数体的 `{ }`
初始化一次。没有哪个声明有两个初始化式，而正是这第二个让它没有 `;` 可收尾——于是接上了上面第一条。
判据写成"声明符已经读到一个 `Initializer` 时，`{` 不是第二个初始化式"，**在这里失败**；失败之后 `ErrorNode`
恢复只吃掉那个宏，`void f(size_type) { }` 随后被正确地读成函数（两半都断言在
`gaps.rs::a_declarator_takes_only_one_initializer` 里）。

**三、一个声明符可以有两个初始化列表，一个分支一个**（`bits/cow_string.h:515`，copy-on-write 的构造函数）：

```cpp
basic_string()
#if _GLIBCXX_FULLY_DYNAMIC_STRING == 0
  _GLIBCXX_NOEXCEPT                       // 宏后缀，只在一个分支里
#endif
#if __cpp_concepts
  requires is_default_constructible_v<_Alloc>
#endif
#if _GLIBCXX_FULLY_DYNAMIC_STRING == 0
  : _M_dataplus(_S_construct(…))          // 初始化列表，也是一个分支一个
#else
  : _M_dataplus(_S_construct(…))
#endif
  { }
```

两处都要改：**指令与宏后缀要交替读**（`#endif` 之后可能是宏，宏之后可能又是指令），以及**初始化列表后面可以
再来一个指令 + 一个 `:`**（`parse_further_member_initializer_lists`）。改之前这个构造函数读不下来，而恢复
**把它的 `{ }` 当成了类的收尾大括号**——`class basic_string` 提前 3400 行结束，它后面每一个成员都被读成文件
作用域的东西（该作用域实测 159 → 48 条事实，再修好之后 **203** 条）。

**教训一：这一族没有一条能靠"报错"发现。** 九处里只有两处产生诊断，其余七处的树无损、良构、零诊断，
只是成员挂错了父亲。判据必须是形状。这也是同一件事第二次收学费（第 6 条约定）。

**教训二：失败的推测解析不会把游标放回去**，所以"先试一次、失败了再补救"要先存检查点。
`bits/stl_vector.h:464` 的 `__glibcxx_class_requires(_Tp, _SGIAssignableConcept)`（宏定义在**被包含的**
`bits/c++config.h` 里，所以"宏表 + 本文件 `#define`"这条证据拿不到它）就是如此：`parse_declaration` 吃掉了名字、
停在 `(` 上才失败，此时"再看一眼当前 token 是不是 `name(`"已经太晚。做法是**在尝试之前**问形状、把
`checkpoint` 存下来，**失败之后**才 `rollback` 去读它——这样"有读法的形状"一个也不会被抢走
（`FOO(x);` 是声明、`TEST(A, B) { }` 是定义、`x = 1;` 是语句，三条都走不到回滚）。已加成维护约定第 32 条。

**教训三（同一条规则的最后一步：它是错的）**：上面那条规则写出来、隔离里全对，量下来却要撤：

```text
declarations_in("std::basic_string")   398 → 287   （闭包；它把真成员吃成了宏）
bits/stl_vector.h 的首错               540 → 540   （为它写的那条规则，在它身上一分钱没买到）
```

**原因**：`ErrorNode` 恢复**只前进一个 token** 再让循环重试，读不下来的那条声明只赔上自己的第一个 token，
**后面的成员仍然是成员**；宏读法把名字**和**括号组一起吃掉，那个括号组是什么构造的开头就一起没了。
所以这一格要的证据是**宏证据**（P3 的宏环境，或一条能读到 `bits/c++config.h` 的来源），**不是形状**。
函数保留在 `decls.rs` 里（`at_a_call_shaped_macro_member`，`#[allow(dead_code)]`），注释里带着这段数字。

**教训四：这一族的收益要看"成员数"，不是"首错行"**。同一轮里 `bits/stl_vector.h` 的首错从 379 推到 541 行，
而 `class vector` **一条事实都没有**——首错往后走只说明"前面那段读通了"，不说明这个类能被问到。
两个数字一起看（第 29 条），再加一个"目标类到底有没有事实"。

**还没做的（下一轮的直接队列）**：

1. `bits/stl_vector.h` 现在的首错是 541 行 `using __do_it = __bool_constant<_S_use_relocate()>;`
   （模板实参里是一个**函数调用**），首错之后还有二十几个错；`bits/stl_map.h` 是 532 行
   `iterator __i = lower_bound(__k);`。两个类的**成员现在都读到了**（`std::vector` 113 条、`std::map` 有
   `begin`/`find` 之外的若干），剩下的是这两个文件里其余的部分。
2. 队列 2.1 里剩下的四处（`bits/move.h:221` 说明符与返回类型之间、`bits/utility.h:176` 别名名与 `=` 之间、
   `bits/alloc_traits.h:48` 类头与基类子句之间、`include/c++/bit:94` requires-clause 与函数体之间）与 `bits/concepts` 一处。
   `bits/stl_iterator.h` 的首错已经是 `: public __detail::__move_iter_cat<_Iterator>`，和 `alloc_traits.h:48` 同形。
3. `bits/basic_string.h` 的首错是 3944 行的 `if _GLIBCXX17_CONSTEXPR (…)` 那一族（队列 2.6：`if` 与它的条件被
   宏/指令切开），与 `if constexpr (requires { … })` 同族。
4. **恢复吃掉块的 `}`**（维护约定第 20 条）：括号本身不配平的瓦砾还是会把类体提前关掉——
   `gaps.rs` 的瓦砾断言里因此**没有**那一段，它属于这一条，不属于已修的那一条。

**"接缝"这个概念本身**：九处之后，"一条规则在等一个特定 token 时，`#` 是它必须接受的前缀"已经不再是猜想。
下一轮值得抽一个 `expect_token_allowing_directives`（**先列全部调用点**，维护约定第 5 条），但**不要**做成
"到处都能跳指令"：现在每一处都是定点的，各有一句"为什么这个 `#` 不可能是别的意思"。

## 标准库那一批（第十一轮）：一个构造写在两个分支里，和"一个 token 定读法"

第十轮之后，队列里剩下的四处接缝有两处**形状已经变了**：`include/c++/bit` 已经干净，`bits/alloc_traits.h` 的首错
也从"类头与基类子句之间"换成了别的东西。这一轮按**当前**的首错做，六条里三条是同一个模式。

**一、一个构造写在两个分支里。** 名字读完之后，`#if` 把它的定义切开：

```cpp
template<typename _Tp, _Tp _Num>
  using make_integer_sequence                     // bits/utility.h:174
#if __has_builtin(__make_integer_seq)
      = __make_integer_seq<integer_sequence, _Tp, _Num>;
#else
      = integer_sequence<_Tp, __integer_pack(_Num)...>;
#endif

template<typename _Tp>
  concept __is_signed_int128                      // bits/iterator_concepts.h:615
#if __SIZEOF_INT128__
      = same_as<_Tp, __int128>;
#else
      = false;
#endif
```

别名的名字与 `=` 之间、concept 的名字与 `=` 之间，都是**没有任何东西能站的位置**，所以那里的 `#` 只能是它自己。
两处共用一个小规则 `parse_a_definition_per_branch`（`= 载荷 ;` 读一遍，`#` 之后再读一遍），构造函数上的那一份
（第十轮修过的 `bits/cow_string.h:515` 的第二个初始化列表）是 `parse_further_member_initializer_lists`。

**二、被指令推开的 clause，和"身体是谁的"。** 模板头自己的规则会读"紧随参数表的 requires-clause"，而库里把它
写在条件里：

```cpp
template<typename _Tp, typename _Up>
#if __cpp_concepts                                             // bits/alloc_traits.h:72
  requires requires { typename _Tp::template rebind<_Up>::other; }
  struct __rebind<_Tp, _Up>
#else
  struct __rebind<_Tp, _Up, __void_t<typename _Tp::template rebind<_Up>::other>>
#endif
  { using type = …; };
```

于是 clause 落到 `parse_declaration` 的**头循环**里（它已经在那儿读指令），加一格"指令之后还可能是 clause"。
同一段还有两处要一起改，因为**两个分支各有一个类头，而身体只有一个**（在 `#endif` 之后）：

* `a_body_follows_the_class_head` 靠"往后找 `{`"回答"这个头开不开身体"，而它**看穿了指令**——第一个头于是把另一
  个分支的 `{` 当成了自己的：身体的规则带着 `#` 进不去，整条声明塌掉，`bits/alloc_traits.h` 的
  `__allocator_traits_base` 与它后面**所有**声明一起没了。判据补一句：**指令之后出现类关键字，就说明那个身体
  不是我的**；
* `parse_declaration` 里"`{` 是不是身体"那一问，改成**说明符自己的事件**里有类头就算
  （`declaration_wrote_a_class_head`）——光标前面只剩一个 `{` 时，从光标往后看什么都看不出来。

**三、一个 token 定读法：模板实参是类型还是值。**

```cpp
using F = std::function<void()>;              // 函数类型：`(` 前面是关键字类型
using C = BoolConstant<_S_use_relocate()>;    // 调用：同一个 `(` 前面是**名字**
```

改前两条都读不出来——`std::function<void()>` 在**任何地方**都读不出来，包括 `std::vector<std::function<void()>>`，
而它大概是 C++ 里最常见的模板实参之一。读法改成两半：空括号**在关键字类型之后**才算形参表
（`a_parameter_list_is_the_type`）；其余情况下类型读法停在 `(` 上就不算读完，实参读取器回落到表达式读法。

**四、`typename` 在表达式里。** `bits/basic_string.h:3944` 的条件是
`if _GLIBCXX17_CONSTEXPR (typename _Alloc_traits::is_always_equal{})`——依赖类型的函数式转换。一元表达式的规则里
没有 `typename` 这一格，于是 `expected primary expression` 指着关键字，整个 `if`（以及它后面的一切）跟着丢。
补一格：`typename` + 一个名字 +（`{…}` 或 `(…)`），读成 `CastExpr`。类型**按名字读**而不是按 type-id，理由是
type-id 会带一个抽象声明符，把这次转换的括号本身吃掉（`typename T::f(int)` 会变成一个函数类型、载荷为空）。

**教训（这一轮最贵的一条，已加成维护约定第 35 条）：`rollback` 只截断，不能"回滚到未来"。**
第三、四条最早写成"类型读法停下之后，若表达式读法也失败，就回滚到类型读法的末尾"——那个检查点是在
`rollback` **之前**取的，而它指向的区间已经被截掉了：`events.truncate(更大的长度)` 什么都不做，于是函数带着
**失败读法**的游标返回 `Ok`，留下的标记成了无法配对的向前引用，`bits/tuple` 直接把建树器打崩：
`forward parent must point at a NodeStart, found Trivia`。一个读法、一次回退。
值得记的是这个缺陷的**形状**：它不是"读错了"，而是"**就地返回了一个错的 Ok**"——静默错树的极端形态，
而且测试与普查都会撞上（`tuple` 在闭包里，`std_probe` 一跑就崩）。

**五、同一条教训在语句层再来一次（这一条把 `std_query` 推到 7/7）。** 第十轮的第 34 条约定说的是
"就地放弃、token 留着的错误路径必须带上 `NodeEnd` 关节点"——那一轮改的是**声明**那一层。语句层的
`parse_expression_statement` 与 `CppParser::recover_to_level` 仍然是**摘掉**（`close_marks_above`），
于是库里最常见的那个写法把它的一半容器带走了：

```cpp
mapped_type& operator[](const key_type& __k) {          // bits/stl_map.h:527
  __glibcxx_function_requires(_DefaultConstructibleConcept<mapped_type>)   // 宏，没有 `;`
  iterator __i = lower_bound(__k);
  …
}
mapped_type& at(const key_type& __k) { … }              // ← 从这里开始不再是成员
```

表达式语句在缺 `;` 处失败、把开着的 `ExpressionStat` 摘掉，那个没配对的 `NodeStart` 于是吞掉了**函数体剩下的
部分（包括收尾的 `}`）以及整个类体剩下的部分**——`std::map` 的成员表在 511 行断掉，`m.find` 答"未声明"。
两处都改成 `end_marks_to`。同一轮里 `parse_stats` 的恢复也不再 `break`（一条读不下来的语句不再让整个块停工：
它跳过 `;`/`}` 之后**接着读**），两处合起来把 `std::map` 的 40 条事实变成 50 条、并且把消息总数从 1318 压到 **932**。

**量到的**：

```text
std_query（examples/std_query.rs）    3/7 → 7/7（七条查询全部答出来）
普查（128 个文件的闭包）              干净 80 / 报错 48；消息总数 1421 → 932
                                      每文件错误数：干净 80 | 只有一个 6 | 两到五个 13 | 超过五个 29
std_index                            声明 10378 → 12550；类型 5326 → 6223（别名 709 → 744）
bits/utility.h 与 include/c++/bit     0 报错
bits/stl_vector.h 的首错              541 → 1865 行；bits/basic_string.h 3944 → 4531 行
bits/alloc_traits.h 的首错            80 → 453 行；bits/iterator_concepts.h 616 → 908 行
```

形状断言四条（`gaps.rs`）：`a_declaration_written_once_per_branch_is_read_as_one_declaration`、
`a_template_argument_may_be_a_call_or_a_function_type`、
`a_statement_the_parser_gives_up_on_keeps_the_block_after_it`，以及"九段接缝"那条里新增的两段。

**还没做的**：

1. 各文件的**下一条**：`bits/move.h:233`（函数体里的 `__glibcxx_function_requires(...)`——**现在只是报一条错**，
   块与类都不再丢，见第 2.3 节）、`bits/alloc_traits.h:453`、`bits/iterator_concepts.h:908`、
   `bits/stl_pair.h:407`、`bits/basic_string.h:4531`、`bits/stl_vector.h:1865`。
2. 队列里**还没碰**的：§2.2（GNU 类型拼写：`__typeof__` / `__int128`）、§2.5（模板参数表里的宏）。
3. 剩下 29 个"超过五个错"的文件——那些是级联，按第 11 条先归类。

## B120：**体是花括号的宏**——位置化的体证据，和"排列顺序就是修复本身"

MSVC 写命名空间的方式与 libstdc++ 不同，而差别正好落在这一条上：

```cpp
// yvals_core.h:1773（无条件）
#define _STD_BEGIN namespace std {
#define _STD_END   }

// <vector>:24（整个文件没有一处字面 `namespace std`）
_STD_BEGIN
_EXPORT_STD template <class _Ty, class _Alloc = allocator<_Ty>>
class vector { … };
_STD_END
```

**例子**（最小、且两条通道各一份）：`_STD_BEGIN struct vector { int size; }; _STD_END`。

**现象**：读成下面这样，而且**零报错**（A0 类——树是错的，但没有一条判据会响）：

```text
Syntax(Declaration)
  Syntax(DeclSpecifierSeq)
    Syntax(TemplateType) > NameExpr("_STD_BEGIN")   ← 宏被读成一个**类型名**
    Syntax(StructDef)                                ← `struct vector` 成了它的说明符的载荷
```

**成因**：两条，各自独立。

1. **证据只问了文件自己**。`body_shapes_the_braces` 原来只问 `macro_body_kinds`（这个文件自己的
   `#define`），而 MSVC 的体在**被包含的** `yvals_core.h` 里——问不到，于是这条规则根本不响。
   按位置问（`macro_body_kinds_at`）之后还要注意**它走哪条通道**：实测 `<vector>` 里 `_STD_BEGIN` 是
   `evidence false | positional body None | in-force body Some("namespace std {")`——体在
   **"条件成立的那一支"**（`yvals_core.h` 的 `#define` 落在条件区里），不是位置化的定义通道。
2. **排列顺序**。规则排在"把名字读成声明头/类型"的那些规则**后面**时，那些规则会先到并且**成功**：
   `_STD_BEGIN struct vector {…};` 是一条说明符为 `_STD_BEGIN`、以 `;` 收尾的完整声明，
   `_STD_BEGIN vector<int> x = {};` 是两个说明符连写。两条都无损、都可能没有报错——**所以这一条的修复
   有一半是"把它挪到前面"**，而那不叫取巧：体是这里最强的证据，它**说出了** token 没说的事。

**性质**：缺信息里的"缺规则"——信息早就在（B86 起 `MacroFact::body_range` 就带着体、B84 起
`MacroEnvironment::body_text_of` 能按位置给），缺的是拿它去读的那一步。

**修法**：`body_shapes_the_braces` 改成按位置问体，并把这条规则**排到 `parse_stat` 的第一条宏规则**。
接受两种体：以 `namespace` 开头的（`namespace std {`、`namespace __8 {`）和**恰好是** `}` 的；
拒绝空体（`#define POINTER_32`）、拒绝命名空间**名**（`#define _GLIBCXX_MATH_NS __8`）、
拒绝 `extern "C" {`（那是连接块的头，构造不是命名空间，它今天由形状规则读，读法不变）。

**量到的**：

```text
MSVC include 树里以花括号结尾的宏体    4 个（_STD_BEGIN、_STDEXT_BEGIN、_EXTERN_C、_TRY_BEGIN）
体单独一个 `}` 的                    6 个（_STD_END、_STDEXT_END、_END_EXTERN_C、_END_LOCK()、_END_LOCINFO()、_CATCH_END）
写 _STD_BEGIN 的文件                 134 个 / 144 次调用，每个文件 begin/end 都配平（0 个不配平）
<p vector>、<string>、<map> 闭包里 _STD_BEGIN 的调用点   39 处（38 处独占一行），后面第一个 token 全是能起声明的
                                     （_EXPORT_STD 17、template 10、#if 5、#pragma 3、using/enum/#ifdef 各 1）
⇒ 语料**量不出**这条规则：38 处里每一处，形状规则都碰巧读对了
```

所以判据只能是形状断言，而它按**四条通道**各写一遍（`gaps.rs` 的
`a_macro_whose_body_is_a_brace_is_read_as_that_construct`）：文件自己的 `#define`、生效中的体（MSVC 的形状）、
定义+体（`macro_evidence` 会答"这是宏"的那条）、以及**后面跟不出声明**的 `_STD_BEGIN vector<int> x = {};`
——只有第四条的答案是关于这条规则的，前三条形状规则也能读对。外加两条否定：没有证据时**一个字都不认**，
以及 B87 的形状读法**没有被动到**。

顺带修的一处 API：`MacroEnvironment::is_empty` 原来只看定义通道，于是一个**只**带生效中宏体的环境会被
判成"空的"——正是这一轮要用的那条通道（`cpp_dump` 的 `--body` 就是被这个坑先绊了一下）。

**改动前后的实测**（探针先修了自己的一个不确定性问题，见 `roadmap.md` §4.2：`-I` 顺序原来取自 `HashSet`，
同一个二进制三次跑出的 seeds 差 10%；修完两次跑逐字节相同）：

```text
455 个文件（钉 CXX=mingw）        报错 454/1、消息 12 条 —— 前后完全一样
    带 --seeds --closure          1590444 seeds / 433 个文件 / 10143073 条条件事实 / 1854806 个体在生效
                                  —— 逐字节一样（只有计时不同）
128 个文件                        128 干净、0 消息 —— 前后一样
MSVC 闭包（`std_query` 的 [where] 那份事实报告）  <vector> 165 条 / <map> 333 条 / <xstring> 129 条 —— 逐字节一样
macro questions（不带 seeds）      38264 → 38260（128 那份 9293 → 9289）
```

最后一行是**唯一动过的计数**，而它动的不是读法：新规则排在那几条"看形状/问宏"的规则**前面**，
于是有 4 处不再去问那个问题了——同一个节点、同一个位置，只是少绕一圈（老路要先把声明读一遍、
回滚、再从 Ok/Err 两条臂里认出来）。这正是 A0 类缺陷的麻烦之处：**改对了，报错计数一动不动**，
所以判据只能是形状断言加上这份逐字节的事实报告。

**还没做的一半（作用域）**：`std` 这个作用域还没开出来，所以 `std::vector` 仍然查不到。名字
（`std`）**不在文件的 token 里**，所以它不会进树，而是走事实那条线：`build_scopes` 在 `_STD_BEGIN` 的调用点上
按体开出作用域、在 `_STD_END` 上关掉，名字落进 `DeclFact.scope`，证据记进摘要——键的问题已定，
见 [`index-design.md`](index-design.md) §"宏体推导出的事实：记证据，不进键"。**树保持扁平**是刻意的：
一旦树的形状依赖环境，"编辑器那棵树"（`FileView::parse`，没有环境）与"索引那棵树"就会不一致，
而两张不一致的树比一张少说了话的树糟得多。

## B121：**体是限定名前缀的宏**（`_STD` = `::std::`）—— 已修复

MSVC 的 STL 里到处是 `_STD vector<int> v;`、`_STD addressof(*_Ptr)`，而 `yvals_core.h` 说
`#define _STD ::std::`。文件里写的是**两个名字**，展开后是**一个限定名**：`::std:: addressof(*_Ptr)`。

MSVC 的 STL 里到处是 `_STD vector<int> v;`、`_STD addressof(*_Ptr)`，而 `yvals_core.h` 说
`#define _STD ::std::`。文件里写的是**两个名字**，展开后是**一个限定名**：`::std:: addressof(*_Ptr)`。

**最小复现**（已钉住）：

```cpp
template <class _Ptrty>
constexpr auto _Unfancy_maybe_null(_Ptrty _Ptr) noexcept {
    return _Ptr ? _STD addressof(*_Ptr) : nullptr;
}
int after;
```

```text
_Ptr ? _STD addressof(*_Ptr) : nullptr        → Error: expected `:`, but get identifier
                                                （`_STD` 被当成类型、`addressof(*_Ptr)` 当声明符 = 又一次
                                                 most vexing parse），`Declaration@0..147` 把 `int after;` 一起吞了
_Ptr ? ::std::addressof(*_Ptr) : nullptr      → 干净（声明规则看到 `::` 就不认，于是读成调用）
```

**成因**：`a_macro_that_is_a_specifier` 只接受"整条体都是说明符"的体（`__declspec(dllexport)`、`const`…），
而 `::std::` 的词类是 `Scope Identifier Scope` —— 一个**嵌套名限定符**，不是说明符。于是体明明拿到了，
也没有规则用它，parser 落回"名字后面跟名字"的声明读法。体不喂也一样，所以这条**与证据无关，是缺规则**。

**在语料里的后果（为什么它值一整条）**：`<vector>` 第 419 行的这个函数体从此吞掉**文件的其余部分**：

```text
Syntax(Declaration)@15422..153477
  Syntax(CompoundStat)@15504..153477        ← 文件总长就是 153477，`}` 再也没配上
    …
      Syntax(Declaration)@19258..153477     ← 类 `vector` 的定义（第 493 行）就在里面
```

类读出来了、成员也读出来了（`std::vector` 作用域里 15 条），但它的**名字**事实落在一个 Function 作用域里
（限定名会跳过透明的 function 作用域，所以成员还带着 `std::`，名字没有）——`std::vector` 因此查不到。

**修法（已落地）**：体**以 `::` 结尾**的宏是嵌套名限定符，它后面的名字属于同一个限定名，而不是声明符。
判据只有一份（`types::a_macro_qualifies_the_name`，问的是体的**最后一个** token 是不是 `Scope`），
两个文法各用它一处：

```text
parse_name（类型位置）           `_STD reverse_iterator<iterator>` 读成一个限定名
parse_primary_expr 的名字段循环（表达式位置）  `_STD addressof(*_Ptr)` 读成一个限定名再跟调用
```

宏调用自身仍是诚实的 `MacroCall(NameExpr)` 节点（树里没有假 token），循环**接着读它限定的那个名字**——
两者之间的 `::` 在替换列表里，本文件根本没有这个 token 可消费、也没有可期待。与 B120 同一族：
**体说什么，读法就跟着说什么**；区别是 B120 管"开一个构造"，这条管"补一段限定名"。

**同一条规则在索引里能生效，还需要一件事**：索引那次 parse 过去**不喂环境**，所以规则永远不触发。
现在 `FileIndexer::with_macro_bodies` 把同一个 `MacroEnvironment` 同时给 parse（`ParserConfig::
with_macros_from_includes`）和作用域遍历（`MacroBodies`），第二遍的触发名集合也从"结构性体"扩到
`BodyShape::a_reading_uses_this()`（体以 `::` 结尾的算在内）。

**量到的（不钉 CXX，MSVC 14.35 的 STL 闭包 109 个文件、`examples/std_query.rs`）**：

```text
std_query                 0/9 → 1/9（`v.push_back -> vector std::vector::push_back`），钉住 CXX 仍 9/9
<xstring> 的事实数         129 → 1181      <p vector> 165 → 650      <p map> 333（本来没被吞）
std::basic_string 的成员表 0 → 155
钉住的四份普查             455/128（带/不带 seeds）逐字节不变
```

**这一轮顺带修掉的两个事实层缺陷**（都是被 B121 的修复照出来的，值得单独记）：

1. **析构函数/运算符的事实原来存空名字**：`fact_for` 用的是 `binding.name.identifier_text()`，而析构函数
   的名字是 `NameKind::Destructor` —— 于是 `DeclFact::qualified_name()` 退化成*它的作用域*，
   每个析构函数都替自己的类作答：`definition("std::vector")` 同时找到类和 `~vector`，答 `Ambiguous`。
   现在存 `Name::text()`（`~vector`、`operator=`），并且 `project::matches` 额外拒绝**无名**事实
   （"没有名字的事实不声明任何名字"）。
2. **`_EXPORT_STD` 的真实体是空的**（`export` 要 `_HAS_CXX23 && _BUILD_STD_MODULE`），而且它在条件区里
   ⇒ 走"生效中的体"通道。这不是细节：无条件定义的同名宏会进**定义**通道，`macro_evidence` 于是有答案、
   `at_a_macro_that_stands_for_a_declaration` 让位，同一行会被读成别的东西。fixture 按实测写成
   "条件里定义成空"（`tests/scopes.rs`）。

## B122：**体是另一个宏的名字**（`_TRY_IO_BEGIN` → `_TRY_BEGIN` → `try {`），以及它连锁弄坏一个类体

**例子**（`<xstring>` 第 523 行，`basic_string` 之前的一个自由函数里）：

```cpp
    } else { // state okay, insert characters
        _TRY_IO_BEGIN
        if ((_Ostr.flags() & _Ostr_t::adjustfield) != _Ostr_t::left) {
```

**现象**：`expected ';' after expression`（524 行，光标停在 `if` 上），然后恢复吃掉一个 `{` —— 于是**整个文件的括号收支错位一格**，
`basic_string`（2444 行）本该在 5047 行的 `};` 收尾，实际一路吞到 5416 行：

```text
[where] xstring 的 std::basic_string 作用域（155 条事实）最后几条：
   5074  swap      5300  string     5301  wstring    5303  u8string
   5308  hash      5410  erase      5416  erase_if
应该结束在 5047 行（源文件里 `};` 那一行）
```

这五个别名（`using string = basic_string<char, …>;` 等）本该在 `std` 里，落进 `std::basic_string` 之后，
`definition("std::string")` 就是"未声明"——8 条 `std_query` 里有 5 条卡在这里。

**成因（两层，都要修）**：

```text
① 体是另一个宏的名字：`iosfwd:27` `#define _TRY_IO_BEGIN _TRY_BEGIN`（`_HAS_EXCEPTIONS` 那一支），
   而 `yvals.h` 里 `#define _TRY_BEGIN try {`。现在 shape_of_a_body 与 parser 的 kinds 判据都只读**一层**：
   看到 `[Identifier]` 就答 Other。缺的是**跟着名字再问一次**（有界跳数 + 去过重防环），
   两条通道都要（证据通道有文本 ✔；文件自己 `#define` 的那条只有 kinds，得另想办法或显式承认不支持）
② 语句位置的块开启者：解析链之后 `_TRY_IO_BEGIN` 的体是 `try {`，首 token 是 `TryKeyword`，
   而 B120 那条规则只收 `namespace` 开头的体（当时量到"语料只写两种"，现在量到第三种）。
   一条语句位置的宏调用（体以 `{` 结尾、首 token 是语句关键字）应当整体读成一条语句，
   后面的 `if` 才是它自己的语句——括号收支因此不再错位（`{` 与 `}` 都在宏体里，文件里一个都没有）
```

**性质**：两层都是"缺规则"，不是缺信息——链与体都在闭包里，`std_probe --macro _TRY_IO_BEGIN` 能看见它。

**顺带说明为什么它值一整条**：一个 524 行的读法错误，代价不是 524 行的诊断，而是**类体到文件尾全错作用域**；
这正是维护约定第 8 条（一个缺陷遮住另一个缺陷）的又一例：`std::string` 找不到的成因在两千行之外的一个 `try` 上。

## B124：**被描述过的宏站在说明符位置**（SAL 注解），以及"证据门"错在哪 —— 已修复

`<xstring>:592`：

```cpp
constexpr bool _Traits_equal(_In_reads_(_Left_size) const _Traits_ptr_ _Left, _In_reads_(_Right_size) const _Traits_ptr_ _Right)
```

``expected `)`, but get const``。`_In_reads_` 是 SAL 注解宏，**在闭包里是有定义的**（`<sal.h>`），
而 `types.rs` 那条"宏站在说明符位置"的规则（B91 的那条）当时要求**谁都不认识这个名字**：

```text
require evidence ... moved NOTHING        （binders.h 的 `_GLIBCXX11_DEPRECATED_SUGGEST` 根本没有证据）
shape alone          → 两个文件倒退        （`WINOLEAPI_(HINSTANCE) CoLoadLibrary (…)`）
现在的代码：macro_evidence().is_none() && macro_body_kinds_at().is_none() && a_specifier_follows_the_group()
```

**出错的是那道门的问法**：它问的是"这个名字有没有证据"，而真正分开两种读法的是**组后面跟什么**——
`WINOLEAPI_(HINSTANCE) CoLoadLibrary (…)` 的组后面是**名字**（那条规则归 `a_macro_call_begins_the_declaration`），
而 `_In_reads_(n) const int *left` 的组后面是**说明符**（`const`）。`a_specifier_follows_the_group` 早就在那儿，
门却仍然按证据关着，于是被描述过的注解宏被让给了"把它读成类型"的那条路。

**修法**：去掉两道证据门，只留 `a_specifier_follows_the_group` 与 `!at_an_attribute`。两文件倒退那条线因此
留在原地（名字在后），而被描述过的注解宏得以通过。

**量到的**：

```text
MSVC 闭包（109 个文件）   干净 73 → **79**（失败 36 → 30）
<xstring> 的首错          592 → **1868**（`_EXPORT_STD _NODISCARD constexpr string_view operator"" sv(…)`）
std::string               从"未声明"变成"**在没能求值的条件后面**"——别名的事实已经落在 std 里了
                          （`<xstring>` 的 std 作用域 48 → 68 条；basic_string 成员表 173 → **202**）
钉住的读数                128/0/0、455=454/1+12、带 seeds 全干净、std_query 钉 CXX 9/9 —— 四份普查逐字节不变
```

**这条同时说明下一个靶子换了层**：`std::string` 现在不是解析问题，而是**条件求值**问题——
`#ifdef __cpp_lib_char8_t` 那一支没能定下来（`__cpp_lib_char8_t` 由 `yvals_core.h` 按 `_HAS_CXX20` 定义），
查询于是答 `ConditionalCompilation` 而不是给出别名。

## B125：**带条件的 `#include` 一律被当成 Conditional**（条件根本没问）—— 已定位，修法试过并被测量否掉

`project.rs` 的 `visible_files`（查询用的可见性走查）：

```rust
let step = match include.guard {
    FactGuard::Unconditional => so_far,
    FactGuard::Region(_)     => IncludeVisibility::Conditional,   // ← 没问条件，只要在 #if 里就算
};
```

**后果**：每个标准头都用特性测试包住自己的 include（`<string>`：`#if _STL_COMPILER_PREPROCESSOR / #include <xstring>`），
于是**整库的每一条事实都是 Conditional**，每条查询都答 `ConditionalCompilation`——哪怕那个条件在查询自己持有的环境里
是**成立**的（`_STL_COMPILER_PREPROCESSOR` 在那儿就是 `1`）。这就是第 6 轮之后 `std::string` 卡住的地方：
别名的事实已经在 `std` 里，只是"在没能求值的条件后面"。

**试过的修法（已写、已量、已撤回）**：把这一步换成问 `index::environment::visibility_at`——
`Active` 保留边、`Inactive` 丢掉边、`Unknown` 才答 Conditional（三值正是那套词汇本身的形状）。结果**更差**：

```text
std::basic_string   从"有候选、可见性 Conditional" 变成"一个候选都没有"（NotDeclaredHere）
                    ⇒ 走查对承载整个类的那条边说了 Inactive
```

所以修法不是"在这里调求值器"，而是"让求值器把这个问题的答案弄对"——`visibility_at`/`macros_at` 那条路
对 `#if _STL_COMPILER_PREPROCESSOR` 给出 `Inactive`，那本身就是一个要单独量、单独修的缺陷。
撤回后读数回到 2/9，四份普查与门禁一字未动。

**第 8 轮量到的（决定性的三个数）**：给 `<string>` 的每条 include 直接问一次求值器——

```text
[vis] yvals_core.h   guard Region(0) -> Active      ← 文件自己的 include guard（#ifndef _STRING_）✔
[vis] xstring        guard Region(1) -> Inactive    ← `#if _STL_COMPILER_PREPROCESSOR`，**它成立**
[vis] cctype         guard Region(1) -> Inactive    ← 同一条区域
```

Region(0) 对、Region(1) 错，而 Region(1) 的条件是 `#if _STL_COMPILER_PREPROCESSOR` —— 那个名字由
**yvals_core.h**（`<string>` 第 9 行就包含它）定义成 `1`。所以求值器把"被包含文件定义的名字"读成了
**未定义**：seed 是**完整**的（"除了我列出的，其它都没定义"），而 `macros_at` 造出来的 state 并没有把
闭包里那些 `#define` 收进去 ⇒ `#if X` 取 0 ⇒ `Inactive`。**这是一个答错，不是一个答不出**——
它把一个成立的条件判成"没编译"，而"没编译"会让可见性走查把整条边丢掉（上一轮那次撤回就是这么变差的）。

**第 9 轮：又量到一个更细的数，并修掉其中一条**

```text
[state] at 182 <yvals_core.h>: _STL_COMPILER_PREPROCESSOR defined=Some(false) _HAS_CXX20 defined=Some(false) uncertain=false
[state] at 239 <xstring>:      _STL_COMPILER_PREPROCESSOR defined=Some(false) _HAS_CXX20 defined=Some(false) uncertain=false
```

`macros_at(<string>, 239)` —— 也就是"读完第 9 行的 `#include <yvals_core.h>` 之后"——把
`_STL_COMPILER_PREPROCESSOR` 报成**确定地未定义**（`uncertain = false`）。这是**答错**的形态：
`<yvals_core.h>` 第 15-17 行在 `#else` 支里就是 `#define _STL_COMPILER_PREPROCESSOR 1`。

**这一轮排除了两个嫌疑**（都是读代码 + 上面这个数一起定的）：

```text
✔ 递归是对的：include 那一支把被包含文件按"粘贴在这里"处理（upto = None），不是用外层的偏移去截
✔ 事实确实会被应用到 state：take_fact 那一支走 apply_fact(state, fact, …)
✘ 所以丢失发生在**那一条 #define 自身**的 reach/应用上（`#ifndef _STL_COMPILER_PREPROCESSOR` → 内层
   `#if defined(RC_INVOKED) || …` 的 `#else` 那一支），下一轮的仪器：把 yvals_core.h 那条 fact 的
   `reach` 打出来
```

**修掉的一条**：`include_visibility` 现在把"文件**自己的** include guard 区域"当成 `Active`——
`SummaryGuards::own_guard` 本来就是为这件事存的（"storing the index is what lets a *walk* extend the same
rule to the facts nested inside it"），而这个走查一直没照它办：`#ifndef _STRING_ / #define _STRING_`
之后再 `#include`，名字已被上一行定义，按条件求值就是"没编译"，于是**整个文件的 include 全被跳过**。
这条修完不动读数（上面那个丢失在别处），但它去掉的是一整类"答错"。

**读数**：不钉 CXX 仍 2/9；钉住 9/9；四份普查与门禁与上一轮逐字节相同。

**第 10 轮：那条 `#define` 根本没被走查看到**

在 `macro_candidates` 的 fact 分支里打印 `_STL_COMPILER_PREPROCESSOR` 的 `guard`/`at`/`reach`——
**一行都没打出来**。也就是说：走查（`macros_at(<string>, 239)`）**从来没有走到 yvals_core.h 的那条 fact**。
配合第 9 轮那两个数（state 答"确定未定义"、`uncertain = false`），嫌疑收窄到两处之一：

```text
(a) yvals_core.h 的 summary 里没有这条 fact（索引那一步就没记下来），或
(b) 走查没进 yvals_core.h —— 但那样 `self.summaries.get(&path)` 会落空、`mark_incomplete()` 会被调用，
    而 `uncertain` 就会是 true（实测是 false），所以 (a) 更可能
```

**根因找到了（第 11 轮，B125 收口）**：`<string>` 的 `own_guard` 是 **`None`**——因为
`detect_guard` 先看 `#pragma once`（它在 `#ifndef _STRING_` **上面**，每个 MSVC 头都这么写），
返回 `Guard::PragmaOnce`，而 `own_guard_region` 当时只接受 `Guard::Macro(_)`。

**已经修掉**：`own_guard_region` 现在两种写法都认（`Guard::PragmaOnce` 时再问一次
`has_a_macro_guard`，它是 `detect_guard` 旁边的新帮手，两者因此不会对"什么算 guard"各说各话）。
这条修的是**答错**：在那之前，`macros_at`/`macro_environment` 那类走查会对文件自己的 guard 区域求值，
发现 `_STRING_` 已被上一行定义，于是**把这个文件的每条 include 都跳过**。

**第二次试接 `visible_files`（也撤回了，这次的数更有信息量）**：即使 `own_guard` 已经修好，
求值器**仍然**对承载 `<xstring>` 的那条边说 `Inactive`——`std::basic_string` 与 `std::string`
的候选**再次全部消失**。而 state 那一边的数是实打实的：`macros_at(<string>, 239)` 把
`_STL_COMPILER_PREPROCESSOR` 报成 `defined = Some(false)`（`uncertain = false`）——**自信的错答案**。
所以修法只剩一条路：把闭包按翻译顺序喂进那个 state（`docs/index-design.md` §条件求值末尾那一格）。
在那之前 `visible_files` 保持"一律 Conditional"：**缺的答案**胜过**错的答案**。

## B126：**`#else` 分支里的代码被按 `#if` 的结论判成"没编译"** —— 已定位，修法两段都写好了（本轮未落地）

第 12 轮的仪器（`macro_candidates` 里打印"进入哪个文件"与每条 fact 的 `reach`）把最后一个环扣上了。
`own_guard` 修好之后：

```text
DEBUG enter .../include/yvals_core.h upto=None                       ← 走查终于进去了（修 own_guard 之前从不进）
DEBUG fact _STL_COMPILER_PREPROCESSOR guard=Region(2) at=527 reach=Inactive
DEBUG fact _STL_COMPILER_PREPROCESSOR guard=Region(2) at=572 reach=Inactive
```

`yvals_core.h` 那一段是：

```cpp
#if defined(RC_INVOKED) || defined(Q_MOC_RUN) || defined(__midl)   // Region(2) 的 #if
#define _STL_COMPILER_PREPROCESSOR 0                              // at=527 ← 这一支没被取，Inactive 是对的
#else
#define _STL_COMPILER_PREPROCESSOR 1                              // at=572 ← **生效的就是这一支**，却也答 Inactive
#endif
```

`include_visibility` 的链式判定：

```rust
for at in summary.guards.conditions_of(region) {
    match holds {
        Some(true) => {}
        Some(false) => return Visibility::Inactive,   // ← 对 #if 那一支对，对 #else 那一支是反的
        None => unknown = true,
    }
}
```

**`#else` 的含义正是"前面都没被取"**，所以 `Some(false)` 对写在 `#else` 里的东西应当是**生效**。
"被编译的那条定义"因此被判成没编译 ⇒ `#if _STL_COMPILER_PREPROCESSOR` 无值 ⇒ 整库 Conditional。

**修法（两段，已写好，未落进树）**：

```rust
// 1) 链式判定里，Some(false) 之前先问"这个偏移是不是落在该区域的 #else 里"
Some(false) => {
    if is_in_an_else_branch(summary, at.region, offset) { continue; }
    return Visibility::Inactive;
}

// 2) 判据本身（branches 已经带 kind 与 body 范围，不需要新 API）
fn is_in_an_else_branch(summary: &FileSummary, region: u32, offset: usize) -> bool {
    summary.guards.conditionals.get(region as usize).is_some_and(|conditional| {
        conditional.branches.iter().any(|branch| {
            branch.kind == crate::DirectiveKind::Else
                && offset >= branch.body.start_offset
                && offset <= branch.body.end_offset()
        })
    })
}
```

修好之后才轮到 `visible_files` 那一行（B125 的第二次尝试）：那时求值器才真能答 `Active`。

**本轮状态**：这两段没落地（第一次替换的锚点没匹配上；随后清理临时仪器时把 `project.rs` 切坏过一次，
已修复回绿色）。`own_guard` 那条修复（第 11 轮）在树里。读数：不钉 CXX 2/9、钉住 9/9、四份普查不变；
tests 1183 / clippy / doc / cpp_dump 全绿。

## B127：B125 的规则已落地（记忆化 + 单向）——不钉 CXX 从 2/9 到 **7/9**

第 13 轮把两件事做完了：`#else` 反转（B126）落地，然后把 `visible_files` 那行接上求值器。结果分两半：

```text
✔ 不钉 CXX：std_query **2/9 → 7/9**（s.size / s.substr / s.empty / v.push_back / v.size / (*p).size /
  arr[0].empty 全部命中；`std::string` 现在解析到 <xstring> 的 `using string = basic_string<char,…>`，
  连 type_of 都对）—— B125 那条链（own_guard → #else 反转 → 求值器 → visible_files）到这一刻才闭环
✘ 钉 CXX：**9/9 → 4/9**，而且**跑了几分钟没结束**（被 kill）
```

原因是实现而不是语义：`visible_files` 走整张 include 图，而**每条带条件的 include** 都去问一次求值器，
`visibility_at` 每次都现建一个闭包 state（`macros_at`）。libstdc++ 的闭包里有几百条这样的边，
一次查询于是把整个闭包走几百遍——正是 `macros_at` 注释里那句"要问成千上万次就该用增量状态"踩中的坑。

**解决代价的办法（第 14 轮，已落地）**：答案**记忆化在索引上**——`ProjectIndex::visibility_answers`，
键是 `(文件, 区域)`，`insert_at` 时清空（摘要一变，任何条件的答案都可能变）。用 `std::sync::Mutex` 而不是
`RefCell`，因为索引要保持 `Sync`（语言服务器把它放在锁后面）。同一个问题在一轮里被每条查询、每次走查各问
一次，而一轮里真正不同的问题只有几百个——记忆化之后那些走查只付一次。

**单向**：只有 `Active` 被*使用*（条件成立 ⇒ 这条 include 变成无条件）；`Inactive`/`Unknown` 都留在
`Conditional`。三值词汇邀请你在 `Inactive` 时丢掉这条边，而这个"没被取"在本会话里**两次都是错的**
（没认出的 own guard、`#else` 里的定义）——单向的规则只会**增加**事实，不会丢。

**落地的读数**：

```text
不钉 CXX   std_query 2/9 → **7/9**（s.size / s.substr / s.empty / v.push_back / v.size / (*p).size /
           arr[0].empty；`std::string` 解析到 <xstring> 的 `using string = basic_string<char,…>`，type_of 也对）
钉 CXX     9/9 保持，探针整轮 19.7 s → **33.7 s**（记忆化之后的代价，可接受但要记着）
四份普查   128 → 128/0/0；455 → 454/1、12 条消息（不变）
门禁       tests 1183 / clippy 0 / doc 0 / cpp_dump 0 error
```

**还剩 2 条**（`m.find`、`m.begin`）：`std::map` 查得到（`<map>`，kind Type，scope `std`），
但它的 41 条成员里**没有 `find`**——MSVC 的 `map : public _Tree<…>` 把 `find` 放在基类里，
所以这 2 条要的是**基类链**那一步（`DeclFact::bases` 与 `member_across_files` 的基类走查），
与可见性这条线无关。
## B128：基类名要**在包围它的作用域里查**——`_Tree` 是 `std::_Tree`。不钉 CXX 7/9 → **9/9**

第 15 轮把最后 2 条（`m.find`/`m.begin`）拆到只剩一环，第 16 轮修好。链条是**四个实测**，前三个都对，
错在第四步的**查法**：

```text
[have] std::map -> map (Type, scope Some("std")) bases ["_Tree<_Tmap_traits<_Kty, _Ty, _Pr, _Alloc, false>>"]
       ✔ 基类记下来了（`DeclFact::bases`）
[members] std::map -> 41 members, 1 unlisted
[members]   unlisted base "_Tree" (`_Tree` is not declared in this file, …)
       ✔ 基类拼写被 `base_type_name` 归一化成 `_Tree`（模板实参、`::`、elaborated 都剥掉了）
[base] std::_Tree -> 126 members, `find`: 4
       ✔ 基类**在索引里**，而且 `find` 有四条事实（<xtree> 里）
definition("_Tree")  -> xtree, scope Some("std")      <-- 名字查得到
declarations_in("_Tree") -> 0 | declarations_in("std::_Tree") -> 126   <-- 成员却按 `std::_Tree` 归档
```

**成因**：两条基类走查（`members_of` 的层级循环、`member_fact` 的单成员循环）都拿 `bases_of` 给的拼写
**照原样**去查——`_Tree`。而 MSVC 的 `<map>` 写的是 `class map : public _Tree<…>`，`_Tree` 在 `<xtree>` 里、
在 `_STD_BEGIN`（= `namespace std {`）里声明，所以事实的 scope 是 `std`，成员按 `std::_Tree` 归档：
`declarations_in("_Tree")` 是 0，`definition("_Tree")` 虽然命中（`matches` 也认裸名），但
`direct_members` 末段的判据是"事实自己的限定名等于问的拼写"，`std::_Tree ≠ _Tree`，于是报
`Unknown(NotDeclaredHere("_Tree"))`——**不是找不到基类，是问错了名字**。

**修复**（第 16 轮）：C++ 里基类名的查找从**包围该类的那个作用域**开始、由内向外；`std::map` 的基类
`_Tree` 就是 `std::_Tree`，而文件作用域的 `_Tree` **不该**被查到（由内向外在第一个有这个名的作用域就停）。
`resolved_in_the_enclosing_scopes(index, scopes, path, owner, base)`：owner 的每个包围作用域拼出
`<scope>::<base>` 依次问 `is_declared`，**最后才是原拼写**（全局名字空间就是最外层作用域）；带 `::` 的
基类原样返回（限定名是关于"名字住在哪"的断言，不该被改写）。两条走查现在都走这一个函数，所以"先缓冲区、
再索引"这件事只决定一次——`resolve_aliases` 早就为别名目标做过**同一条规则的同一小步**，并在那里写着它。

**形状断言**：`crates/cpp_code_analysis/src/index/project.rs`
`a_base_of_a_class_in_a_namespace_is_looked_up_in_that_namespace`——两条走查都钉（列表 + 单成员），
带一个负例：查询文件里另有一个文件作用域的 `_Tree { void wrong(); }`，`m.wrong` 必须**仍然查不到**
（更近的作用域有这个名字，查找就停在那里，不会落到外面那个同名类）。

**落地的读数**：

```text
不钉 CXX   std_query 7/9 → **9/9**（m.find -> xtree std::_Tree::find，m.begin -> xtree std::_Tree::begin）
           `std::map` 的成员表 41 + 1 unlisted → **135 members, 0 unlisted**（find ×4 在 depth 1，声明于 std::_Tree）
钉 CXX     9/9 保持，答案一字不变（m.find -> stl_map.h std::map::find：libstdc++ 的 `map` 自己就声明 `find`，
           这条走查它本来就不需要——所以钉住的读数测不到这条规则，可它必须不变，实测不变）
四份普查   128 → 128/0/0；455 → 454/1、12 条消息（不变）
门禁       tests 1184 / clippy 0 / doc 0 / cpp_dump 0 error
```

## B129：**偏特化的前置声明**与主模板撞同一个限定名，`definition` 报 `Ambiguous`

量出声的一张新缺口，与 B128 同一条查询链上、但不是它的一部分：

```text
[have] std::vector -> unknown: `std::vector` is declared more than once in what this file can see
         candidate: vector name="vector" qualified="std::vector" (Type, scope std, offset 19311, Unconditional)
         candidate: vector name="vector" qualified="std::vector" (Type, scope std, offset 94905, Unconditional)
```

两处都在同一个文件 `<vector>` 里：19311 是主模板定义 `class vector { … }`，94905 是
`template <class _Alloc> class vector<bool, _Alloc>;`——**偏特化的前置声明**。`base_type_name`
把模板实参剥掉（这是它对基类和对 `DeclFact.type_of` 一律要做的），于是两者的限定名都是 `std::vector`，
而事实里没有任何字段说"这条声明带模板实参 / 这条只是声明"，索引**没有依据**分辨它们，只能报歧义。

**性质**：`Ambiguous` 的语义是"两个**不同实体**共用一个拼写，语言拒绝替你选"，而前置声明与定义是**同一个
实体**——所以这里是"报得过头"，不是"报错了"。**当前无害**：探针的 `v.push_back`/`v.size` 走的是对象的
`type_of` → `member_fact`，两条事实的成员一起被取，9/9 实测不受影响。要真正修好，得在事实里记下"这条
声明的模板形参列表是什么"（或至少"这是个偏特化"），那是**新字段**、要抬 `FORMAT_VERSION`，不该顺手塞进
一轮里。

libstdc++ 那侧同形（钉 CXX 的读数，两条都不是本轮引入的）：

```text
[have] std::map        -> unknown: `std::map` is declared more than once in what this file can see
[have] std::map::find  -> unknown: `std::map::find` is declared more than once …
```

`m.find` 照样答 `stl_map.h std::map::find`（`member_fact` 的逐层走查不受影响），所以**同一个"报得过头"
只出现在名字查询上**——这也说明它和 B128 是两件事：B128 是"问错了名字"，这条是"名字对了但有两条声明"。

## B130：别名的成员表**少了目标的基类**（B128 的同形，但**没落地**）

**性质**：从代码读出来的结构缺口，**未实测**（这一轮的门禁读数都是在它未修的状态下测的，所以留作队列条目）。

`members_of("std::string")` 的两半走的是两个拼写：

```text
自己的成员  direct_members 里 resolve_aliases("std::string") -> "std::basic_string"  ✔ 对
基类        bases_of(..., "std::string")  —— 用的是**别名**那个拼写
```

而 `DeclFact.bases` 对别名是空的（`declarations.rs`：`declared_bases_of` "Empty for anything that is not a
class"）：`using string = basic_string<char>` 没有基类子句。于是别名那一层的走查**立刻停下**——
MSVC 的 `basic_string : public _String_val<_Val_types>` 这一级的成员不会出现在 `std::string` 的成员表里
（`std::basic_string` 自己的表不受影响，探针的 9 条也都不需要它，所以看不到症状）。libstdc++ 的
`basic_string` 没有基类，钉住的那一侧本来就没有这一步。

**最小修法**（一行，但会改行为，所以留给单独一轮 + 一条形状断言）：走查的**入口**也用解析后的拼写算基类，
而 `declared_in`（level 0）仍旧报问的那个拼写：

```rust
let named = resolve_aliases(index, scopes, root, path, class);   // 只为走查取基类
let mut level = bases_of(index, scopes, root, path, &named) ...  // owner 也用 named
```

## 维护约定
1. **修好一条**：把本文档的条目改成"已修复"（保留成因与修复过程，下一个人会需要），并写进 `crates/cpp_parser/tests/gaps.rs` 的已支持清单。`gaps.rs` 的机制是"构造一旦开始工作，钉住它的测试就会失败"，那是防漏报的护栏。
2. **发现新缺漏**：先加进本文档（带四要素：例子、现象、成因、性质），需要护栏时再加进 `gaps.rs`。本文档是队列，`gaps.rs` 是回归。
3. **标了"取舍"的不要动**。如果非动不可，先在这里写清楚为什么值得推翻原先的决定。**反之亦然**：标了取舍的条目如果被证明"其实不需要查找"，就该像 T1 那样改掉，别让一个错误的取舍判断挡住一条能修的规则。
4. 语料探针是找缺漏的手段，不是缺漏的记录处，它必须保持 **0 error、0 ErrorNode**，所以发现缺漏时**不要**把坏构造留在里面。（原先的探针 `crates/cpp_parser/examples/corpus/constructs.cpp` 与 `examples/dump.rs` 已从仓库删除，改用 `cargo run -p cpp_parser --bin cpp_dump -- <file>` 与 `crates/cpp_parser/tests/real_world.cpp`；探针文件本身随时可以从 git 历史里取回。**加 `--tree` 时即使有错也把树打出来**——排查缺规则时最需要的正是"它把这个构造读成了什么节点"，而默认只在干净时才打。）
5. **改公共入口的读写规则时**（例如让某个 token 在 `parse_expr` 里多一种含义），必须同时列出所有**自己拼这串 token** 的规则并逐一验证。C2 的修复在 `cargo test` 全绿的情况下弄坏了 GNU case 区间和 lambda 捕获列表，两个都是靠语料库和抽查才发现的。
6. **改的是"某个构造读成什么"时，同时加一条 `gaps.rs` 的形状断言**。报错、无损、良构三条判据都拦不住错树（见 A0）；只有"这个构造必须读成这种节点"能拦住。加断言的成本是几行，漏掉它的成本是 A0-1 那样——静默地错在几乎每个函数体里。
7. **kind 表里有节点、规则里没有产出**，是一张空头支票（`ParenExpr`/`LambdaExpr` 长期如此，`RequiresKeyword` 直到 C1 才兑现）。要么兑现，要么别在表里留。
8. **一个缺陷会遮住另一个缺陷**（第 13 项）：`x = {1};` 曾经靠 A0-1 的错树"通过"，A0-1 修好后它变成响亮报错，花括号初始化列表缺规则这件事才露出来。所以修好一条之后，**把它的邻居再走一遍**——新露出来的缺口往往不是新坏的，而是一直错着，只是从前错得安静。同理，改完一条规则要问的不是"测试还绿吗"，而是"**它以前替谁挡着**"。第 13 项里 A0-2 就是这么被找出来的：为了给 requires-clause 补第四个位置而逐条探针，撞上了 `template <typename T> void f(T) { }`——一句和 concept 毫无关系的写法。
9. **"旁边有个同名判据"不等于能复用**（A0-2）：`declarator_starts_with_a_type_keyword` 与 `a_type_keyword_precedes_the_declarator_name` 问的看起来是同一件事，实际一个看**整条声明的第一个 token**、一个看**声明符名字前面的那个 token**。模板头正好卡在两者之间，于是最需要修的那条写法落在前者的判据之外。**复用判据之前先问它从哪里开始看**——起点不同，答案就不同。
10. **一个词如果同时是名字，它就不该是 token 种类**（B12）。`requires`、`concept` 当初进了关键字表，代价是 `int requires = 1;` 这种完全合法的程序读不出来；而它们真正的判据从来不在词法层——"这里是不是 clause"要看后面跟着什么。凡是要按上下文判定的词，**词法器交给标识符、语法层按拼写判**，`module`/`import`/`final`/`override` 一直如此。反面教材还有一个细节值得记：C1 里那张"空头支票"（`RequiresKeyword` 在 `is_expression_keyword` 里却没有规则消费它）最后不是靠补规则解决的，而是发现**那张表本来就不该收它**——表里的每一条都要能回答"哪条规则消费它"。
11. **拿真实文件当探针，而且要按"同一成因"归类报错**（第 16 项）。手写清单只能覆盖"想得到的构造"，真实文件覆盖"实际存在的构造"，两者交集之外的才是漏网之鱼：`(size_t)size`、十个相邻字符串字面量、`for (;; i++)`、`1_km` 四条没有一条是手写清单能想到的，而它们全在同一个 120 行文件里。归类同样重要——那个文件报了 13 条错，按成因分只有 3 个根因，其余全是**级联**（一条读错，后面整段跟着错）。**先归类再动手**，否则会在级联上浪费时间，也容易把"级联"误当成"很多个缺陷"。
12. **写反面用例也会撞出缺口**（B18）。B16 的测试里要证明"后缀字面量不参与拼接"，写了个 `"a"_km` 当反例——结果它根本读不出来：词法器特意产出了 `UserDefinedLiteral`，而语法层没人接。这是"空头支票"的**镜像版本**：不是"表里有节点没规则产出"，而是**上游特意分出来的东西，下游没人接**。测试里那些"为了说明边界"的输入，值得当成探针来对待。
13. **"解析成功"不是证据，要问"读完停在哪、后面跟着什么"**（B19、B21，以及 T1/T2 两次推翻）。这个 parser 里反复出现的判据错误是同一句话：把"这条规则没报错"当成"这就是对的读法"。四次修补的判据形状完全一样——
    * `sizeof` 的类型读法必须**停在 `)`**（B19）；
    * cast 的 `(` 后面那个 `)` 后面**不能跟操作数**（T1）；
    * 声明符的名字是 template-id 时后面**必须是 `::`**（裸 template-id 那条）；
    * template-id 读完之后**不能跟操作数**（B21）。
    
    写新规则时先问这两句：**它应该在哪个 token 上收尾？收尾之后那个 token 允许是什么？** 只写"解析成功就接受"的规则，在这个容错 parser 里迟早会在垃圾输入上"成功"。
14. **同一条判据的第二处用法，当场抽出来，别写第二遍**（B22）。"后面跟着操作数"这条证据先用在 cast 上（T1），后来用在 template-id 上（B21），两处都需要同一条例外（clause 内不生效）——第二次直接改的时候漏了 cast 那处，是**新写的测试用例**（`requires (C<T>) T value = T{};`）把它抓出来的。抽成 `an_operand_is_decisive` 之后，例外只有一处实现。**"这条例外要加在哪里"是比"这条例外是什么"更容易错的问题**：只要同一个判据出现两次，例外就有两处可能被漏。
17. **符号查询是三值的，而且本文件优先**（外部符号表）。kind_of(name) -> Option<SymbolKind> 里的 None 是"**这张表不知道**"，绝不是"不是类型"；"否"必须由 Some(其它种类) 表达（Function 用来否决声明读法）。判据的顺序固定为**本文件（TypeNames/MacroNames）→ 外部表 → 形状偏好**，因为索引是滞后的、而"文件自己说的话"就是正在解析的文本。另一条同样重要的：**表只决定读法、不决定结构**——任何表（包括胡说八道的）都必须产出无损、良构的树，	ests/symbols.rs 用一张"一切都是宏"的表把这条钉住了。接口定义在 crates/cpp_parser/src/symbols.rs。
16. **把"约定"换成"证据"，只在能拿出证据的地方放宽**（B41）。宏的判据一开始是**拼写约定**（全大写下划线就算宏）——它能用，但它是个猜测：CHECK(x) 漏写分号也会被当成宏吞掉。改法是**建表**：#define 过什么名字是文件里写着的事实，parser::MacroNames 把它记下来（和 TypeNames 同形、同一边界），于是"宏调用省略分号"这条本该吞手误的规则变成了**只对本文件定义过的宏生效**，g(x) 漏分号照旧报错。分工要记清楚：**需要证据的地方用表（放宽的代价是诊断），兜底的地方用约定（反正两种读法都是错，挑损失小的那个）**——所以"宏 + 块"那两处（B32/B36）保留拼写兜底，头文件里来的 TEST 依然读得出来。判据一旦放宽，**反向钉住**必须同时加上：gaps.rs 的"仍不支持"清单里钉着反例，放宽就会立刻失败。
15. **嵌进去的规则会先花掉外层规则要的 token**（B24 的第二个缺陷）。K&R 形参表读的是**真正的声明**，每条自带一个 `;`，于是外层声明收尾时那个 `;` 早被吃掉了；而这只在"没有函数体"的形状上暴露——有体时游标落在 `{` 上，走的是另一条分支。所以看到"这里应该有个 `;`"时，要先问**这段 token 里有没有嵌套规则已经消费过它**。同族问题还有 `friend`：它的载荷是整条声明（`;` 在内），外层当初也又找了一遍 init-declarator，失败后回退，把后面的成员全变成了错误节点。两次的形状一样：**外层以为收尾符号还在**。

18. **跨层按形状写的判据，要在本文档留痕**（成员访问读成 `IndexExpr`）。`w.size` 与 `arr[0]` 产出的是**同一种节点**（`IndexExpr`），区别只在运算符 token 是 `.` 还是 `[`——`MemberExpr`/`ArrowExpr` 这两个种类存在，但 `w.size` 不走它们。索引层的成员访问查询因此按**运算符文本**判断，而不是节点种类：`crates/cpp_code_analysis/src/sema/resolve.rs` 的 `member_access_of`。谁要是把 `w.size` 改成产出 `MemberExpr`，必须同时改那里，否则**成员访问会静默地全部失效**（不是报错，是每个 `obj.member` 都变成"游标不在成员访问上"）。这类"下游读上游形状"的耦合要在这里记一笔，因为它的失败模式是静默的，而本文档是读法唯一的登记处。

19. **类体里的特殊成员函数有两种形状，都不走 `init-declarator`**（`~Widget();` 与 `S();`）。由成员列表那一轮查出来，两个症状都静默：

    ```text
    ~Widget();            Declaration[Declarator[NameExpr(~ Widget), ParameterList]]       没有 DeclSpecifierSeq，
                                                                                          也没有 InitDeclarator
    Widget();             Declaration[DeclSpecifierSeq[TemplateType[NameExpr(Widget)]],   名字被读成了**类型**，
                          InitDeclarator[Declarator[ParameterList]]]                      声明符里只剩 ()
    virtual ~Widget();    Declaration[DeclSpecifierSeq[VirtualSpec],                      形状正常 —— 唯一
                          InitDeclarator[Declarator[NameExpr(~ Widget), ParameterList]]]   走得通的一条
    ```

    * **下游症状一（已修）**：`name_from_text` 用 `descendants_with_tokens` 找 `~` 与 `operator` 关键字，而它被传的是**整条声明**。于是 `struct Widget { ~Widget(); };` 的声明（那个 `StructDef`）里也有一个 `~`，**类自己**被读成了析构函数 `~Widget`：类的绑定没了、作用域被命名成 `~Widget`、析构函数之后写的每个成员都落进那个作用域。真实 C++ 里几乎每个类都有析构函数，所以这不是边角。修法是把这两个 token 的查找范围收到**名字节点**上（`declared_name` 传 `&name_node`），回退路径才继续传声明节点。`tests/scopes.rs` 的 `a_class_with_a_destructor_in_its_body_keeps_its_own_name` 与 `a_class_with_an_operator_in_its_body_keeps_its_own_name` 各钉一条，`index::project` 的 `a_destructor_without_a_specifier_is_not_a_member_yet` 钉住下游那一半。
    * **下游症状二（未修，已知缺口）**：`CppDeclaration::get_name()` 走的是 `init-declarator`，上表前两种形状都没有它，于是 `is_unnamed_declaration` 判定"这条声明没有名字"并丢掉整条声明——**构造函数、没有 `virtual` 的析构函数、`= delete`/`= default` 的特殊成员都不是绑定**。修它要的是一条"裸 declarator 也算声明了名字"的规则，而这条规则必须同时不接受 `is_unnamed_declaration` 存在要拒绝的形状（`tests/scopes.rs` 的 `a_call_statement_declares_nothing` 与 `a_real_declaration_is_still_declared` 是这条规则两侧的钉子）。边界现在由 `a_destructor_without_a_specifier_declares_nothing_yet` 钉住：**能力落地那天它会失败，改它就是有意为之**。

    这一条与第 18 条同族，但更贵：第 18 条只是"读错了运算符"，这一条是**把一个声明读成了另一个实体**，而且顺带吞掉了它后面的成员。

20. **成员访问运算符后面什么都没有时，恢复吃掉了块的 `}`**（**已修**）。做补全入口时探到的：`w.` 是补全**唯一真正被问到的状态**（运算符打完、名字还没写），parser 当时的读法是

    ```text
    w.          IndexExpr[IdentifierExpr(w) Dot]   + 恢复从 `Err` 走
    w.si        IndexExpr[IdentifierExpr(w) Dot Identifier(si)]   + 只报 "expected `;` after expression"
    ```

    形状本身**够用**——对象和运算符都在 `IndexExpr` 里，所以成员列表 / 补全在**那个游标处**照常工作（这正是第 19 条里"形状能用就先别动 parser"的同一判断）。问题在**恢复的半径**，实测（`void f() {\n  Widget w;\n  w.\n}\nint after;\nvoid g() { }\n`）：

    ```text
    Errors: ["expected identifier after member access operator"]
    Declaration@0..54
      CompoundStat@9..54          <- 本该在 29 结束
        Declaration@13..25        <- Widget w;
        ExpressionStat@25..30     <- w.\n}\n   ：IndexExpr 把块自己的 `}`(28..29) 吃了
        Declaration@30..41        <- int after;     ** 于是它变成了 f() 的局部 **
        Declaration@41..54        <- void g() { }
    ```

    对照 `w.si`（同一个位置、同样报错）时结构是**完好**的：`CompoundStat@9..32`，`int after;` 仍是文件作用域。差别只来自"`parse_expr` 用 `Err` 返回"这条路径。

    **修法**（就是当时写下的那条）：成员名缺失时补一个零宽节点、`push_error`、`complete` 之后 **`break` 出后缀循环**，不要让 `Err` 穿过语句层。落在 `parse_postfix_suffixes` 的成员访问分支里，用的是 `parse_compound_stat` 对"缺 `}`"的同一范式（`emit_missing_node()` + `push_error`）：**正在被编辑的位置不是失败构造**，它既该有一个节点给补全住，也该有一条诊断告诉用户。

    **量到的**：两个语料（128 个 libstdc++ 文件、455 个分析闭包）**一个数字都没动**——系统头里没有半写完的 `w.`。它的收益在**光标**上，不在语料里：`gaps.rs::a_member_access_without_a_name_keeps_the_block_after_it` 是它的钉子（改回 `Err` 立刻变红），断言三件事：诊断仍在、`int after;` 仍在、它**不在**函数体里。

21. **标准库闭包是最有说服力的探针语料，用它的"第一个错"当队列**（`docs/std-library.md`）。第 11 条说"真实文件覆盖实际存在的构造"，标准库是这种语料能拿到的最好的样本：本机闭包 185 个文件 / 111k 行，**只有 26% 干净解析**，而修它要按 **include 图的拓扑序**（叶子在前）——因为上层需要读的宏与声明来自下层。用它的时候有两条判据必须守住：

    * **按文件数报进度，不按报错条数。** 标准头级联得厉害：`bits/stl_algobase.h` 一个构造没读出来，后面挂 190 条错。所以"2067 条 unexpected token"不是 2067 个缺陷，甚至不是 2067 个现象。
    * **看每个文件的第一个错**，那是唯一没有东西能解释它的一条。`cargo run --release -p cpp_code_analysis --example std_probe -- files.txt` 打的就是这个视图，附带"这个错所在行有没有闭包定义的宏"的占比——**这个占比是判断"该建表还是该修语法"的依据**，本机是 58% 对 4%（只有 4% 是本文件自己 `#define` 的），于是结论很硬：文件自己的 `MacroNames` 对标准库几乎没用，起作用的是外部符号表（第 17 条那条线）。

    还有一条**不要踩的坑**：标准库的宏在 `#if` 两个分支里可以展开成完全不同的东西（`_GLIBCXX_BEGIN_NAMESPACE_CONTAINER` 在一个分支里开出 `namespace`、在另一个里什么都没有），所以"知道它是宏"并不总能换来"跳过它就行"——开出作用域的那种只能**展开**。这是第 15 条（嵌套规则花掉外层的 token）的同族：**跳过与展开的差别，在"宏里有括号"时就是错树与对树的差别。**

22. **头文件名是一整个 token，而"里面能出现什么"不能用白名单回答**（已修）。`#include <vector>` 在 parser 里是一次**折叠**：`CppParser::try_lex_header_name` 把 `<`、名字、`>` 这一串合成一个 `HeaderName` token。它当初只接受这几种 token：`Identifier | Dot | Slash | Minus | Plus | IntegerLiteral`——而 `c++config` 的词法是 `c`、`++`、`config`，`PlusPlus` 不在表里：

    ```text
    #include <vector>            -> Token(HeaderName) "<vector>"                    折上了
    #include <bits/c++config.h>  -> Less Identifier Slash Identifier PlusPlus …      没折
    ```

    而 `bits/c++config.h` **是 libstdc++ 里最常见的一条 include**：几乎每个标准头都写它。

    **代价不是树难看，是目标错了。** 分析层对"没折上"的尖括号包含有一条兜底（`preprocess/directive.rs` 的 `parse_include`），它从 token 重建名字——**从 `<` 本身开始拼**，于是目标是 `<bits/c++config.h`，解析不到任何文件。而"include 解析不出来的摘要不落盘"是 `index::store` 的规则，所以每个包含它的头都**不会被缓存、每次会话重新解析、里面的声明全部不可见**。实测一个 `<vector>` 闭包：169 个文件里 **54 个落不了盘**、62 条 include 解析不到；两处修好之后同一个闭包是 309 个文件（链走通了，闭包本身变大）、只剩 **2 个落不了盘**、3 条解析不到。

    两处都修了，各有各的钉子：

    * **折叠的白名单换成黑名单**：只有"行尾"能打断它（`Newline`、`LineContinuation`、`Eof`），因为标准里 h-char 是"除换行和 `>` 之外的任何字符"。白名单在这里是把**"词法器会产出哪些 token 种类"当成了"头文件名里能有什么"**，它永远会漏——`gaps.rs` 的 `a_header_name_is_one_token_whatever_is_in_it` 钉住，包括"新行仍然结束它"和"指令之外 `<` 仍然是小于号"两条反向断言。
    * **兜底从 `<` 之后开始拼**，并保留分隔符之间的 trivia——那之间的文本**就是**名字，空格是名字里合法的字符。`tests/preprocess.rs` 的 `an_unfolded_angle_include_names_the_file_between_its_delimiters` 钉住（用 `#  include <…>` 触发这条路径：`#` 与指令名之间有空白时 parser 不尝试折叠，所以这条兜底永远够得着）。

    这一条与第 18 条同族但更隐蔽：**形状看起来没坏**（树仍然无损、良构、没有错误节点），坏的是一个 token 的**文本**，而它只在"有人拿这个文本去解析一个文件"时才发作——跨了两层。发现它靠的是把闭包真的索引一遍（`examples/index_includes.rs`），不是读代码。

    顺带一条**没修、记在这里**的观察：`CppLexer::lex_header_name`（词法器那个入口）现在**没有调用者**——parser 自己做 token 级折叠——而它的文档写着"预处理层会带着游标调用它"，那句是假的，它的规则和现役那条也不完全一样。按第 7 条（表里有、规则里没有产出，是空头支票）它该被兑现或去掉；那是独立的一次改动，不该混在修 bug 里做。

23. **来自头文件的宏站在声明位置上：判据是"后面跟的东西能不能开始一个新构造"**（已修，48→78 个文件干净）。标准库里两处最常见，而且**任何宏表都帮不上忙**——名字 `#define` 在 `c++config.h` 里，那是**被包含**的文件，本文件的 `MacroNames` 没见过，外部表也没接到 includes 上（为什么那条线要单独一轮，见 `docs/std-library.md` 的"计划修正"）：

    ```text
    namespace std _GLIBCXX_VISIBILITY(default)    每个 libstdc++ 头的第一行；展开成 __attribute__ 或什么都没有
    _GLIBCXX_BEGIN_NAMESPACE_VERSION              独占一行；一处分支展开成 namespace __8 {，另一处什么都不是
    ```

    于是只有形状可用，而形状在这两处是决定性的：**名字与 `{` 之间**合法 C++ 什么都没有（两种读法是"`namespace std {`"与报错），**一行上的孤立名字**要么是宏要么是漏了分号——后者只在函数体里才比前者更可能。两条规则加上去，`bits/stl_algobase.h` 从 190 条错降到零头，因为它那 190 条全来自第 83 行的命名空间头。

    **规则被改写了一次，两次的错都值得记**，因为两次都是"看起来对、其实把另一种读法吃了"，而且**都是既有测试抓住的**：

    * 第一版问"名字后面还有没有 **declarator**"。`x = 1;` 是它的代价：`=` 后面没有 declarator，于是赋值被读成宏、`=` 左边空了。正确的问题不是"后面能不能接声明符"，而是"后面跟的东西能不能**开始**一个新构造"——`=`、`*`、`(`、`.`、`[` 都是**延续**，所以它们必须在表外。这就是第 13 条那句话的又一次应验：**问"读完停在哪、后面跟着什么"，而"后面能跟什么"要按"它是要延续还是要另起"来分类，不是按"它长得像不像声明符"**；
    * 第二版把 `FOO(x);`（最令人头疼的解析，本项目读成声明）和 `TEST(A, B) { … }`（块就是体的定义）也吃了。两处都由别的规则拥有且各有测试，所以括号组后面是什么现在由 `kind_after_the_group` **一次走查**回答，四个调用方共用（第 14 条：同一条判据的第二处用法当场抽出来——这次是第四次用法，抽晚了）。

    `gaps.rs` 的 `a_macro_from_a_header_can_stand_where_a_declaration_goes` 把**四个反例**钉在一起（赋值、最令人头疼的解析、宏定义、函数体内的漏分号），因为"把这些再次吃掉"正是下一次放宽最容易犯的错。

    **补充（同一轮的后半）：这条规则读的是一串名字，不是一个名字。** libstdc++ 会连写两个，有时三个：

    ```text
    _GLIBCXX_BEGIN_NAMESPACE_VERSION
    _GLIBCXX_BEGIN_NAMESPACE_CONTAINER
      template <typename> struct _List_iterator;
    ```

    停在名字后面的**第一个** token 会答"后面是个标识符"，于是一个答案在三个 token 之外的形状被拒绝。所以扫描器跳过一串"名字 + 它自己的括号组"，再问那个 token 能不能开始一个声明。加这一层时自己带进来两个错误，都由实测数字发现、现在都有反例：

    * `index_after_the_group` 已经返回"组的 `)` **之后**"，调用方却又跳过一个 token（`next_significant_index` 是"再下一个"）。于是 `size_t _Hash_bytes(const void*);` 里那串名字被扫过头，两个名字都成了宏调用，`;` 落到了下一条声明上——`bits/hash_bytes.h` 由干净变成报错。修法是把"从 index 起跳过 trivia"抽成 `significant_index_at`，`next_significant_index` 用 `index + 1` 表达它（第 14 条：一处实现）；
    * 扫描器**从不看游标自己那一对括号**：第一轮循环先问"后面是不是标识符"，于是 `_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(_V2)` 直接答出 `LeftParen`、被拒绝——`system_error` 由干净变成报错。修法是把"跳过括号组"放在循环的**开头**，第一轮就处理游标自己那个名字。

    两次的形状是同一条教训：**扫描器的起点与终点各差一个 token，症状都是"某个文件从干净变成报错"，而只有把闭包整个跑一遍才看得见。**

24. **编译器自己的属性拼写就是属性**（已修）。`__attribute__ ((…))`（GNU）与 `__declspec(…)`（MSVC）和标准里的 `[[…]]` 是**同一个概念、同一批位置**，而标准库正是用前一种写的：

    ```text
    template <typename _Tp>
      __attribute__((__always_inline__))          bits/move.h：模板头之后、声明之前
      inline _GLIBCXX_CONSTEXPR _Tp* __addressof(…)
    extern "C++" __attribute__ ((__noreturn__, __always_inline__))   bits/c++config.h：说明符序列里
    void terminate() _GLIBCXX_USE_NOEXCEPT __attribute__ ((__noreturn__,__cold__));   参数表之后
    ```

    修法是**一个**谓词 + 一个解析函数：`at_an_attribute` 认得三种拼写，`parse_attribute_specifier` 把它们都收进同一个 `AttributeList`——一个概念一个节点，消费者不必知道文件是给哪个编译器写的。三处调用点（模板头之后、说明符序列、声明符后缀）各自用的是既有的 `parse_attribute_specifiers`，所以一处改动同时生效。

    **这里按拼写匹配是有依据的，和第 16 条批评的"拼写约定"不是一回事**：这两个名字由标准保留给实现，任何 `#define __attribute__` 的程序都不在需要读的范围里；而且它是**编译器**的扩展，不是文件的——没有任何头文件里的 `#define` 能把它变成别的东西。判据要同时满足这两条才算数，`MY_API` 两条都不满足（所以它继续走"表 → 兜底约定"那条路）。

    属性内部的括号是**平衡 token 组**，不是语法：`__attribute__ ((__mode__ (TI)))`、`__attribute__ ((__format__ (gnu_printf, 1, 2)))` 的内容是编译器的事，这里一个字都不解释。GNU 拼写的双括号不需要特例——平衡计数把它们当普通嵌套，外层那对才是属性的界。

25. **declarator 的**后缀**位置上，标识符只有两种读法**（已修，一次拿下 17 个报错文件里的 5 个）。libstdc++ 在几乎每条声明后面都放一个宏：

    ```text
    inline void __terminate() _GLIBCXX_USE_NOEXCEPT
    T* addressof(T& r) _GLIBCXX_NOEXCEPT { … }
    bool before(const type_info&) const _GLIBCXX_NOEXCEPT;      cv 限定符之后
    void f() _GLIBCXX_NOEXCEPT_IF(noexcept(g()));                带参数的
    extern "C" void abort(void) _GLIBCXX_NOTHROW _GLIBCXX_NORETURN;   连着两个
    int x MY_DECL_SUFFIX;                                        变量名之后
    ```

    在这两个位置（函数 declarator 的后缀、变量 declarator 的名字之后），一个标识符**只有两种读法**：上下文关键字，或者宏。其余合法的东西全是关键字或标点——`{`、`;`、`=`、`,`、`:`、`[[`、`->`、`noexcept`、`const`。所以接受一个名字不花任何合法程序，而另一种读法是报错；这是第 16 条那个"兜底"侧，和 `eat_namespace_head_macros` 同一立场。

    **三个名字必须被显式拒绝**，而且各有各的理由：`override` 与 `final` 是**合法的**上下文关键字（同一个循环里本来就在处理它们），`requires` 开启一个**子句**——那条子句由 declarator 自己的循环读（`decls.rs` 的 `at_requires` + `starts_a_requires_clause`），在这里当成宏会把约束吞掉、把它的 token 留到下一条声明上。`gaps.rs` 的 `a_macro_can_stand_among_a_declarators_suffixes` 把这三条拒绝和六个正例钉在一起。

    一处助手、两个调用点（`eat_function_qualifiers` 与 `finish_init_declarator`），因为这是同一个位置的两个实例：一个在参数的 `)` 之后，一个在名字之后。**写第二遍才是错的**（第 14 条）——两处的判据完全相同。

    顺带记一个**没有**被这条规则救回来的形状：`__atomic_flag_data_type _M_i _GLIBCXX20_INIT({});` 仍然报错，但它**不是**这条规则没生效——是更早的一步（"这是声明还是表达式"）因为 `({})` 这个括号组选了表达式读法。队列要按**第一个错**排而不是按"哪条规则没生效"排，否则会去修一条根本没走到的规则。

26. **运算符名在表达式里也是一个名字**（已修）。C++ 允许把一个运算符函数**按名字调用**，标准库用它问"这个类型能不能比"：

    ```text
    { operator<=>(static_cast<_Tp&&>(__t), static_cast<_Up&&>(__u)); }     compare：requires 表达式里的光杆调用
    { operator<(std::forward<_Tp>(__t), std::forward<_Up>(__u)); }         bits/ranges_cmp.h，同上
    { return operator~() + 1; }                                            bits/max_size_type.h：无参调用再参与运算
    { t.operator<(u); }   p->~T()   s.~basic_string()                      点号/箭头之后的运算符名与析构名
    ```

    三个位置各是一个分支，所以一个能跑不代表另外两个能跑：

    * **光杆的**：`parse_primary_expr` 的名字分支列了 `Identifier | Scope` 却**没列 `OperatorKeyword`**——于是 `operator<` 只在 `::` 之后被当成名字（`Foo::operator+()` 一直是通的），在表达式开头不是。它一直没被发现，是因为**函数体里那个形状由声明规则读掉**：`operator<(a, b);` 被读成一条转换运算符声明，无损、无错、形状不同——所以这个洞只在 requires 表达式里露出来（那里只有表达式读法）。**这是第 18 条那类"跨层形状耦合"的又一个变体：一个分支在另一个分支的影子里，测试只覆盖了影子里的那个。**
    * **点号之后的运算符名**：成员访问的读取只收 `Identifier`，遇到 `operator` 直接报"expected identifier after member access operator"。修法是调用**声明侧那同一个** `parse_operator_name_here`（它的文档早就写着"暴露出来是因为表达式的 callee 与声明的名字是同一个问题"）——一处实现，两个位置。
    * **点号之后的 `~`**：伪析构调用 `p->~T()`。`~Name` 在表达式里也是名字的一种拼法，而且后面还可能带限定名或模板实参（`x.~A<T>()`），所以读取用的是表达式自己的名字读取器。

    `gaps.rs` 的 `an_operator_name_is_a_name_in_an_expression_too` 把三个位置和四种拼法钉在一起。

27. **"读错了"和"没读到"是两种失败，量的时候要分开数**（做 `DeclFact::clean` 那一轮）。第 13 条说"解析成功不是证据"；这一条是它的另一半：**解析失败也不是"事实是错的"的证据**。给每条声明标"读得干不干净"时试了十几个坏文件片段，结果是**绝大多数坏声明根本不产生事实**——`int broken = ;`、`int f(int a = );`、`class C { int m; } c = ;`、`int x, y = ;` 全是零条事实：声明符一步失败，符号层就什么都没有，于是没有"错的事实"可标，只有**缺失的事实**。所以：

    * **计划要按这两种失败分**：标准库答不出 `std::vector` 是"没读到"（解析缺口，靠 P1 修），不是"读到了但不敢信"（那是 `clean` 的事）。把两者混成一件事，就会去修错的那一层——本文档的 `std-library.md` 里"不变量 3 的粒度才是真正挡着语义查询的那道门"就是这么写错的；
    * **粒度取舍不能只比数量，要问"多出来的那些是什么"**：`clean` 的判据有三条候选，在 `<vector>` 闭包里分别判 106 / 214 / 2907 条不可信。决定第二条的不是"214 比 2907 小"，而是把差集**按节点种类拆开**：多出的 108 条里 89 个是 `Declaration`、19 个是 `TemplateDecl`——正好是**类型部分**与**模板头**（声明符之外、声明之内，而 `type_of` 就是从那段文本读的），所以要买；而 2907 那一条多出来的是**类体里一个错连坐每个成员**，所以不能要。同一个数字换个拆法就是另一个结论。

28. **判据读"最后一个 token"时，别在它外面加一个以标识符结尾的新说明符**（本轮 `__cdecl`）。`parse_one_decl_specifier` 判断"这个说明符有没有命名类型"，靠的是**它消费的最后一个 token 是不是标识符/`::`/`>`/类型关键字**——于是新加一个以标识符结尾的说明符（`__cdecl`、`__extension__`）会让它答"命名了类型"，而这个答案决定了**后面那个名字是类型还是声明符的名字**。症状的分辨率很高，值得记：`__forceinline size_t f() { }` 报错，而 `__forceinline int f() { }` 正常——差的就是 `size_t` 是一个**名字**。这类"标志是从 token 反推的"判据（第 9 条那句"起点不同，答案就不同"的另一种形态）改成"让调用方自己处理"往往比改判据便宜：这里的修法是把编译器的关键字挪到**那个函数外面**消费，判据一行没动。

29. **一轮的收益要数两个数字**（同上）。标准库那轮把 `__cdecl` 的 1464 处**静默错树**改对了，而"干净文件"只多 1 个——因为那些文件本来就"解析成功"。普查（`std_probe` 的 `clean`/`failing`）看得见报错，**看不见错树**；`gaps.rs` 的形状断言看得见错树，却看不见"还有多少文件没读下来"。两边都要量，否则会以为一轮白做，或者以为一轮做完了。

30. **一个计数器只回答一个问题**（第九轮第一条）。`TypeNames` 的深度同时被当成"名字可见性"和"在不在 body 里"，两者在链接规范的块上分开了：它是作用域（名字在里面、也在外面可见），但**不是** body。用错的那一侧的代价是静默的——`is_inside_a_body` 说"在 body 里"，于是两条按设计拒绝在 body 里生效的宏规则一起失效，而症状出现在**别的行**上（`using ::wint_t;` 报"expected `;` after expression"）。所以：**当一个计数/标志被第二个调用方读走时，先问它问的是不是同一个问题**；不是就再开一个（或者，像这里一样，删掉那个不该有的递增）。

31. **向回走的判据要知道什么"包着"这个构造**（第九轮第三条）。`declarator_starts_with_a_type_keyword` 从游标往回找"这条声明的第一个 token"，一路走过 `;`/`{`/`}` 才停——于是模板头（`template <…>`）被走过去了，而它是**包着**声明的东西，不是声明的一部分，收上来的"第一个 token"是头的 `<`。同一个形状在第 9 条里出现过一次（那次是**起点**不同，这次是**终点**不同），两次都是"判据的边界不在它以为的地方"。写这类扫描时问三句：**从哪开始、到哪停、中间有什么是"包着"的**。

32. **一次失败的推测解析不会把游标放回去**（第十轮第二条）。`parse_declaration` 是**可以失败**的——语句层与成员层都靠"试一次、失败就换一种读法"工作——但失败**不等于回退**：它可能已经消费了若干 token、发出了若干事件、留了几个没关的标记，游标停在它失败的地方。第十轮那一处（`bits/stl_vector.h:464`，`__glibcxx_class_requires(_Tp, _S…)`，宏定义在被包含的 `bits/c++config.h` 里）正是这样：名字被吃掉了，游标停在 `(` 上，此时"再看一眼当前 token 是不是 `name(`"已经太晚。写法是**在尝试之前问形状、把 `checkpoint` 存下来，失败之后才 `rollback` 去读它**（`parse_class_body_members` 里的 `call_shaped`）。这条与第 13 条是一对：那一条说"解析成功不是证据"，这一条说"解析失败也不是回到了原处"。

33. **问"后面跟着什么"时，答案的类别往往是"不能出现在声明符里"，而不是一张拼写表**（第十轮第 9 条与它的两处同形）。`a_declarator_still_follows_the_name` 原来只认 `Identifier`/`*`/`&`/`&&`，于是三处真实代码掉了：跟随者是**类型关键字**（`_GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR` / `bool` / `empty()`）、是**说明符关键字**（`… _GLIBCXX20_CONSTEXPR` / `inline basic_string<…>` / `operator+(…)`）、是 **`operator`**（`_If_sv<_Tp, basic_string&>` / `operator=(…)`）——三处的后果完全一样：名字被当成声明符，成员变成变量，后面的成员一起丢。给这张表加拼写是治标；判据本身应该是"这个 token 可能是声明符的一部分吗"，而 `is_type_specifier_keyword` + `storage_or_function_specifier` + cv 限定符**恰好就是"不可能在声明符里"的那一类**，而且它们已经在别处存在（第 14 条：别写第二张表）。

34. **"就地放弃、token 留着"的错误路径必须把节点关掉，而且要带上结束事件**（第十轮后半段，这条最贵）。`close_marks_above(base)` 是**摘掉**（detach）：它把节点从开着的栈里拿掉，**不发 `NodeEnd`**，指望"主人以后会补"——而对一条已经 `return Err` 的规则来说，没有主人了。那些没配对的 `NodeStart` 由建树器在**流的末尾**补上结束，于是这条被放弃的声明**吞掉了写在它后面的所有 token**：

    ```cpp
    struct Base {
      int x = 1        // 少一个 `;`：声明读到这儿才放弃
      int after;       // ← 成了它的子孙：树里还有，但不再是成员
    };
    ```

    这不是"少读了几个 token"，是**整个类体从这一刻起消失**：`bits/stl_vector.h:192` 那一条成员让 `std::vector` 一个成员都没有（`v.size`/`v.push_back` 都答"未声明"），`bits/cow_string.h:515` 那一条让 `class basic_string` 提前 3400 行结束。判据很简单，**写错误路径时问自己一句：这条路径是"token 也不要了"还是"token 留着"**——前者用 `close_marks_above`（调用方随后 `rollback`，事件被截断，什么都不欠），后者必须用 `end_marks_to`（按身份、逆序、带 `NodeEnd`）。`gaps.rs::a_member_the_parser_gives_up_on_keeps_the_members_after_it_members` 把四段瓦砾钉住了。

    第十一轮接着在**语句层**又付了一次同样的学费：`parse_expression_statement` 与 `CppParser::recover_to_level` 也在"token 留着"的路径上摘节点，于是 `bits/stl_map.h` 那个没有 `;` 的宏把 `operator[]` 的函数体**和整个类体剩下的部分**一起吞了——`std::map` 因此没有 `find`。判据是同一句话，只是层的名字换了一个：**这条路径是"token 也不要了"还是"token 留着"**。

    **第十三轮第三次收费，这次在链接块上**（B44）：`parse_linkage_block` 的成员循环也在"token 留着"的路径上摘节点。区别是这一次的代价不再是"一个类没了"，而是**整份文件的条件结构没了**——一个失败的声明吞掉后面所有声明，连带吞掉链接块的 `}`，于是一个 `CompoundStat` 覆盖 387 000 字节，里面八条 `#endif` 再也不是指令节点。三个层级（类体、语句、链接块）各一次，判据一次都没变，所以**这一条应该写在每个"继续解析"的循环旁边**，而不是只写在 `end_marks_to` 的文档里。

    同一件事的另一面：**恢复"只前进一个 token"是有价值的性质**，值得为它让路。它让读不下来的声明只赔上自己的第一个 token，后面的成员照旧；所以第十轮那条"把 `name(...)` 整体读成宏"的规则（隔离里完全正确）被**撤掉**了——它把名字和括号组一起吃，代价是闭包里 111 个成员（第 29 条：数字先于直觉）。

    同一件事的另一面：**恢复"只前进一个 token"是有价值的性质**，值得为它让路。它让读不下来的声明只赔上自己的第一个 token，后面的成员照旧；所以第十轮那条"把 `name(...)` 整体读成宏"的规则（隔离里完全正确）被**撤掉**了——它把名字和括号组一起吃，代价是闭包里 111 个成员（第 29 条：数字先于直觉）。

35. **`rollback` 只截断，回不到"未来"**（第十一轮那条最贵的教训，被建树器抓了个正着）。`Checkpoint` 记的是事件长度与游标位置，而 `rollback` 做的事是 `events.truncate(...)`——**大于当前长度时它什么都不做**。所以"先试着读 A，不行就回到 A 读完时的状态"这种写法是错的：那个检查点指向的区间在回退到起点时就已经被截掉了，回退到它只剩下**标志位**被恢复、事件却是 B 的残骸，函数于是带着 B 的游标返回 `Ok`，而 B 留下的开标记成了无法配对的向前引用。症状在建树器里：`forward parent must point at a NodeStart, found Trivia`（`bits/tuple` 当场崩）。

    同一个道理的正确写法只有一种：**一个读法、一次回退**；要"两个读法都试"，就在回退之后**重新读**第一个（解析是确定性的，再读一遍拿到的是同一棵子树）。这一条与第 13 条、第 32 条是一族：**"返回 `Ok`"必须意味着"光标停在读法真的结束的地方"**——第 13 条问的是"它停在哪个 token"，这一条问的是"这个 token 是哪一次读法的"。

36. **`rollback` 要回到的不只是事件流**（第十二轮）：`Checkpoint` 里那几样**不在事件流里**的状态必须一样一样地回去——类型名、限定标志，以及**诊断**。前三样都是"一个错的答案被树形状解释不了"之后才找出来的；诊断这一样是**按契约**找出来的，而且量出一个诚实的结论。

    一个读法被丢掉时，它为这个读法报的问题不是关于文件的事实，而是关于一个猜测的事实：留着它，编辑器就会给一段**用户没写过的代码**划线。修法是把 `errors.len()` 也记进 `Checkpoint`、在 `rollback` 里截断。**量到的**：插桩跑遍两个语料（583 个文件）加 53 段片段，**没有任何输入走到过这条路上（0 次）**——也就是说它是一个**契约修复**，不是症状修复。所以它的钉子不能是文件形状的：`cpp_parser` 里直接驱动 parser 的三条单测（`try_parse` 丢诊断 / 保留读法的诊断照留 / `Checkpoint` 的四样状态都回得去），把那条契约钉住；同时 `CppParser::with_text` 被抽出来，二十个字段的构造从两处重复变成一处——那本身也是这一条的一部分：**不在事件流里的状态越多，两处构造漂移的代价越大**。

    **顺带一条做法**：这一轮的缺口（B42/B43）是拿**一堆合法的 C++ 片段**过 parser 量出来的，任何报错都值得看一眼。用它的时候有一条纪律：**片段要先过一遍编译器**——三条候选里有一条是我们对了、片段写错了（`sizeof (T) (x);`，g++ 也拒绝），而"我们的 parser 报错"和"代码本身是错的"长得一模一样。







