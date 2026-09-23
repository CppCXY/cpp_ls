# 语法缺漏登记表

本文档记录**当前 parser 读不了的 C++ 构造**，以及每条的性质、成因和处置计划。

它存在的理由和 `crates/cpp_parser/tests/gaps.rs` 是同一个，只是回答的问题不同：那个文件回答"哪些构造**会**工作"，是回归护栏；这里回答"哪些**不**工作、为什么、打算怎么办"，是工作队列。构造一旦修好，就把它从这里删掉、写进 `gaps.rs` 的已支持清单。

## 判定原则

本项目放弃上下文式解析，不等符号表。这决定了缺漏分两类，处置方式完全不同：

- **缺规则（missing rule）**——语法本身能判定，只是规则还没写。这类**都能修**，只是成本不同。
- **缺信息（missing information）**——从这段代码的 token 里无论怎么看都定不下来，需要跨编译单元的类型索引。这类是**刻意的取舍**，不是待办。

每条都会标注属于哪一类。标了"取舍"的不要试图消灭它，那会走回 clang 的老路。

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

### 下一批（B36–B40，待办）：复验后剩下的 5 个成因

两条都逐条最小复现过，成因已确认，只是还没动手。按"覆盖的文件数"排序：

| 编号 | 例子（最小复现） | 现象 | 归属文件 |
|---|---|---|---|
| **B36** | `void f() { IF_EXIST(k) { g(); } }` | 报 `expected }, but get ;`。**函数体内**的"宏 + 块"——B32 的规则**故意**不收体内（那里 `g(x) { }` 是真错误），但 `IF_EXIST(...) { ... }` 这种"条件宏 + 块"在体内到处都是 | `LuaStyle.cpp`（68 条，全是它的级联） |
| **B37** | `static const struct { unsigned char left; } priority[] = { { 1 } };` | 报 `expected a declarator name`。**无名类类型 + 数组声明符 + 初始化式**（`static const struct { … } name[] = { … };` 是 C 的老写法） | `LuaDefine.h`（45 条） |
| **B38** | `void f(const std::function<bool(TokenKind)> &predicated);` | 报 `expected primary expression`。**函数类型出现在形参/模板实参位置**（`bool(TokenKind)` 是类型不是调用） | `SyntaxNodeHelper.cpp/.h`、`LuaSyntaxNode.cpp`（3 个文件） |
| **B39** | `struct S { void f() { for (auto &v: vec) { } } };` | 报 `expected ;, but get )`。类体内的函数体里，范围 `for` 的 `:` 被当成**位域宽度**读走了（`v: vec`） | `LSP.h`（5 条） |
| **B40** | `try { g(); }` 换行 `#if !defined(_DEBUG)` 换行 `catch (const E& e) {` | 报 `expected }`。指令落在 **`}` 与 `catch` 之间**——B23 那一族（构造中间的指令）的第三种形状 | `IOSession.cpp`（5 条） |

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

### T4. 跨编译单元的类型信息

`TypeNames` 表是**文件局部**的。头文件里的类型、模板参数、没见过的 builtin，它一概不知道。这不是 bug，是"不等符号表"的直接推论。表的定位见 `crates/cpp_parser/src/parser/type_names.rs` 的模块文档，用法见 `cpp_parser/src/grammar/cpp/types.rs` 的模块文档（"Everything in this module is *syntactic*. Name lookup is deliberately absent"）。

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
| — | 函数体内的"宏 + 块"（`IF_EXIST(k) { … }`；B32 的规则故意不收体内） | B36 | 半天 | 待办 |
| — | 无名类类型 + 数组声明符（`static const struct { … } name[] = { … };`） | B37 | 一天 | 待办 |
| — | 函数类型出现在形参/模板实参位置（`std::function<bool(T)> &pred`） | B38 | 一天 | 待办 |
| — | 类体内函数体里的范围 `for`：`:` 被当成位域宽度 | B39 | 半天 | 待办 |
| — | 指令落在 `}` 与 `catch` 之间（B23 族的第三种形状） | B40 | 半天 | 待办 |
| — | 函数 try 块 `void f() try { } catch (...) { }` | B25 | 半天 | 待办 |
| — | `namespace` 与名字之间的属性 | B3 残留 | 半天 | 待办 |
| — | `void()` 作表达式 | B5 | 半天 | 待办（很少见） |
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

## 维护约定

1. **修好一条**：把本文档的条目改成"已修复"（保留成因与修复过程，下一个人会需要），并写进 `crates/cpp_parser/tests/gaps.rs` 的已支持清单。`gaps.rs` 的机制是"构造一旦开始工作，钉住它的测试就会失败"，那是防漏报的护栏。
2. **发现新缺漏**：先加进本文档（带四要素：例子、现象、成因、性质），需要护栏时再加进 `gaps.rs`。本文档是队列，`gaps.rs` 是回归。
3. **标了"取舍"的不要动**。如果非动不可，先在这里写清楚为什么值得推翻原先的决定。**反之亦然**：标了取舍的条目如果被证明"其实不需要查找"，就该像 T1 那样改掉，别让一个错误的取舍判断挡住一条能修的规则。
4. 语料探针是找缺漏的手段，不是缺漏的记录处，它必须保持 **0 error、0 ErrorNode**，所以发现缺漏时**不要**把坏构造留在里面。（原先的探针 `crates/cpp_parser/examples/corpus/constructs.cpp` 与 `examples/dump.rs` 已从仓库删除，改用 `cargo run -p cpp_parser --bin cpp_dump -- <file>` 与 `crates/cpp_parser/tests/real_world.cpp`；探针文件本身随时可以从 git 历史里取回。）
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
15. **嵌进去的规则会先花掉外层规则要的 token**（B24 的第二个缺陷）。K&R 形参表读的是**真正的声明**，每条自带一个 `;`，于是外层声明收尾时那个 `;` 早被吃掉了；而这只在"没有函数体"的形状上暴露——有体时游标落在 `{` 上，走的是另一条分支。所以看到"这里应该有个 `;`"时，要先问**这段 token 里有没有嵌套规则已经消费过它**。同族问题还有 `friend`：它的载荷是整条声明（`;` 在内），外层当初也又找了一遍 init-declarator，失败后回退，把后面的成员全变成了错误节点。两次的形状一样：**外层以为收尾符号还在**。
