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

### B13. 不带 `extern` 的显式实例化

```cpp
template void f<int>(int);          // expected <, but get void
template int v<int>;                // 同上
template class C<int>;              // 同上
```

**性质**：缺规则。**成因**：`template` 后面直接跟类型说明符时，`parse_template_head` 要求 `<`，于是整个声明被回退。带 `extern` 的那半（`extern template …`）已经能读——B10 修的就是它，而两条路走的是**不同的代码**：`extern template` 在声明规则里就地消费两个关键字，裸 `template` 则先撞上模板头。

**处置**：半天。级别 B。

### B14. 限定名 + 无名裸类型形参

```cpp
void Widget::draw(T) { }        // expected primary expression —— 报错
void Widget::draw(int) { }      // 读得出
void Widget::draw(Canvas&) { }  // 读得出
void Widget::f(T t) { }         // 读得出
void Widget::f(T);              // 读得出
```

**性质**：缺规则（与 A0-2 同源，但这一支是响亮的）。**成因**：限定名 `Widget::draw` 被 specifier 序列整段当成类型吃掉（`a_qualified_name_is_the_type`），声明符**自己没有名字**；无名声明符 + 裸类型无名形参这一组合下，形参读法失败而初始化器读法也不成立，于是整条声明回退、语句层再失败。

**为什么先不动**：这是 A0-2 那条判据够不到的角落——A0-2 修的是"有类型关键字时不许走初始化器优先"，而这里有类型但类型是**限定名**（不是关键字），另外还叠着"无名声明符"这个状态。要修得先想清楚无名声明符的形参列表该挂在哪里，属于 A0-1 那个"声明符没名字"家族的后续。

**处置**：半天。级别 B（报错，无静默风险）。

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

### T1. C 风格转换（cast）—— 已修复一半

```cpp
auto d = (int)1.5;       // 已修复（关键字类型，本来就能读）
auto d = (T*)p;          // 已修复 —— 曾被判为"取舍"，那个判断是错的
auto d = (MyType*)p;     // 已修复 —— 文件里根本没声明 MyType 也能读
auto d = (MyType)1.5;    // 仍不读 —— 这才是真正的取舍
```

**原先的记录**把整条判为取舍，理由是"`*` 既是指针声明符又是乘号，`(a*b)` 和 `(a* b)` 的区分需要跨翻译单元的类型索引"。**这个判断错了，而且错了两次**：

1. `(a * b)` 与 `(MyType*)p` **不是同一个形状**——前者的 `*` 两侧都有操作数，后者的 `*` 左边什么都没有；
2. 更关键的是，**紧跟 `)` 的 `*` 根本不可能是二元运算符**，因为二元运算符必须有右操作数。

所以"括号内容以 `*`/`&`/`&&` 结尾"是一个**只看 token 就能确定**的形状，不需要类型表。

**修复**（`exprs.rs::closes_with_a_pointer_operator`）：`is_a_type_in_parentheses` 增加第三种确定形状——括号内容以 `*`、`&`、`&&`（含 `* const` 这类带 cv 的写法）结尾。两阶段的 cast 分支早就存在，缺的只是这一条判据。

**为什么两阶段是必要的**：`(a)` 能解析成 type-id（一个名字、没有声明符），所以"能试就试类型读法"会把每个括号变量变成 `a` 的 cast。第一阶段只是廉价的"值不值得试"，第二阶段（试 cast、失败就回退成括号表达式）才是判据——**这也是为什么第一阶段不必精确**。

**残留的取舍**：`(MyType)1.5`、`(MyType)x`。`(MyType)` 既是合法括号表达式又是合法 type-id，只有名字查找能分辨。顺带地 `(f)(x)` 与 `(T)(x)` 是同一串 token，两者都读成**调用**——那是保留实参的读法。

**方法论修正**：取舍条目当初是按"看起来难不难"判断的，不是按"它真的需要文件外的信息吗"。`(MyType*)p` 因此被搁置了两轮，而它其实是一条半天就写完的规则。已在"刻意的取舍"章节开头加了提醒。

`gaps.rs` 里指针形式已移入"能读"清单并加了 `CastExpr` 形状断言；裸名字形式仍留在"不读"清单。

### T2. `x = a < b > c;` 读成 template-id

`a_matching_angle_bracket_follows` 的浅扫描会被没有空格的 `a < b > c` 骗到，读成模板实参列表。文档（`types.rs::a_matching_angle_bracket_follows`）里写明了取舍方向：**失败模式是良性的**（token 还是被解析了，只是读成了 template-id 而非比较），不值得为它做完整表达式分析。

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
| — | 不带 `extern` 的显式实例化 | B13 | 半天 | 待办 |
| — | 限定名 + 无名裸类型形参 `void Widget::draw(T) { }` | B14 | 半天 | 待办 |
| — | `namespace` 与名字之间的属性 | B3 残留 | 半天 | 待办 |
| — | `void()` 作表达式 | B5 | 半天 | 待办（很少见） |
| — | `(MyType)1.5`（裸名字 cast）、`(f)(x)` 的调用读法 | T1 取舍的那半 | — | **不做** |
| — | `asm volatile`、`__attribute__` | D | — | **不做** |

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
4. 语料探针 `crates/cpp_parser/examples/corpus/constructs.cpp` 是找缺漏的手段，不是缺漏的记录处。它必须保持 **0 error、0 ErrorNode**，所以发现缺漏时**不要**把坏构造留在里面。
5. **改公共入口的读写规则时**（例如让某个 token 在 `parse_expr` 里多一种含义），必须同时列出所有**自己拼这串 token** 的规则并逐一验证。C2 的修复在 `cargo test` 全绿的情况下弄坏了 GNU case 区间和 lambda 捕获列表，两个都是靠语料库和抽查才发现的。
6. **改的是"某个构造读成什么"时，同时加一条 `gaps.rs` 的形状断言**。报错、无损、良构三条判据都拦不住错树（见 A0）；只有"这个构造必须读成这种节点"能拦住。加断言的成本是几行，漏掉它的成本是 A0-1 那样——静默地错在几乎每个函数体里。
7. **kind 表里有节点、规则里没有产出**，是一张空头支票（`ParenExpr`/`LambdaExpr` 长期如此，`RequiresKeyword` 直到 C1 才兑现）。要么兑现，要么别在表里留。
8. **一个缺陷会遮住另一个缺陷**（第 13 项）：`x = {1};` 曾经靠 A0-1 的错树"通过"，A0-1 修好后它变成响亮报错，花括号初始化列表缺规则这件事才露出来。所以修好一条之后，**把它的邻居再走一遍**——新露出来的缺口往往不是新坏的，而是一直错着，只是从前错得安静。同理，改完一条规则要问的不是"测试还绿吗"，而是"**它以前替谁挡着**"。第 13 项里 A0-2 就是这么被找出来的：为了给 requires-clause 补第四个位置而逐条探针，撞上了 `template <typename T> void f(T) { }`——一句和 concept 毫无关系的写法。
9. **"旁边有个同名判据"不等于能复用**（A0-2）：`declarator_starts_with_a_type_keyword` 与 `a_type_keyword_precedes_the_declarator_name` 问的看起来是同一件事，实际一个看**整条声明的第一个 token**、一个看**声明符名字前面的那个 token**。模板头正好卡在两者之间，于是最需要修的那条写法落在前者的判据之外。**复用判据之前先问它从哪里开始看**——起点不同，答案就不同。
10. **一个词如果同时是名字，它就不该是 token 种类**（B12）。`requires`、`concept` 当初进了关键字表，代价是 `int requires = 1;` 这种完全合法的程序读不出来；而它们真正的判据从来不在词法层——"这里是不是 clause"要看后面跟着什么。凡是要按上下文判定的词，**词法器交给标识符、语法层按拼写判**，`module`/`import`/`final`/`override` 一直如此。反面教材还有一个细节值得记：C1 里那张"空头支票"（`RequiresKeyword` 在 `is_expression_keyword` 里却没有规则消费它）最后不是靠补规则解决的，而是发现**那张表本来就不该收它**——表里的每一条都要能回答"哪条规则消费它"。
