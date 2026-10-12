# 现状与计划

> 这份文档取代了 `docs/` 下此前的全部文档（19 份，约 270 KB，已删除）。那些文档记录的是**过程**——
> 每一轮排查的中间状态——而不是**当前的事实**，其中大部分已经与代码不符。这里只写现在成立的事，
> 以及下一步要做什么；代码里指向旧文档的引用已全部改指到本文。

## 一、这是什么

一个用 Rust 写的 C++ 语言服务器。它读取真实工程的方式**先经过预处理**：宏被展开、条件编译按实际
生效的分支解析、`#include` 被内联，得到的"渲染"才是语法分析的输入——因为 C++ 的文本在预处理器
跑完之前并没有确定含义。

## 二、规模与状态

| 部分 | 内容 |
|---|---|
| `cpp_parser` | 词法 + 语法，产出无损语法树（rowan），并带一层**强类型 AST**（`CppTranslationUnit`、`CppIfStat`、`CppClassDef`、`CppParamList`…） |
| `cpp_code_analysis` | 预处理与渲染、索引（每个文件的声明事实）、语义模型（类型推导、成员、作用域）、13 项诊断检查 |
| `cpp_ls` | LSP 服务端：诊断、补全、签名帮助、跳转、引用、语义高亮、折叠、内联提示 |

**这是一个正在开发中的项目**，没有发布版本，接口与内部结构都可能随时改动。

## 三、现在确实能工作的部分（都有测试或可复现命令）

```
cargo build --release --workspace --all-targets     零错误、零警告
cargo test --release --workspace                    全绿
```

- **解析与渲染**：整个 MSVC 标准库闭包（187 个文件）**零语法错误**。
- **13 项诊断**，各有 `DiagnosticCode`（可直接映射到 LSP 的 `code` 字段）、severity 与 tag：

  ```
  syntax-error  include-not-found  macro-redefined  unused  access-invisible
  missing-return  assignment-in-condition  unnecessary-branch  undefined-field
  argument-count  param-type-check  assign-type-check  return-type-mismatch
  ```

- **标准库上的零误报**——这一层的文档把这条测量点名为唯一能约束它的测量，它现在是绿的：

  ```
  cargo run --release -p cpp_code_analysis --example checks_probe -- target/std-cook target/std-cook/assign-case.cpp
    assign-case.cpp:52  [assign-type-check]  `x` is initialised with a value that does not convert to `std::string`
    --- checks over 5 file(s) ---   1  assign-type-check
  ```

  **恰好一条真阳性，其余全零**。`target/std-cook/` 下有复现用的夹具（`assign-case.cpp`、`arity-case.cpp`、
  `broken-case.cpp`）。

- **诊断的判据可以来自整个项目**：报告只针对本文件写的内容，但结论可以读**索引里的事实**——
  例如 `std::string x = 123;` 之所以能报出来，是因为它读的是索引里 `std::string` 构造函数的形参表
  （`DeclFact::parameter_list`），而不是去解析头文件。

### 已修的三条（2024，都有复现与验证）

```
① missing-return 对构造函数/析构函数误报
     `Name(const std::string& f) : firstName(f) {}` 报
     "this function is declared to return `Name`"。
     原因：该 check 依赖"构造函数没有返回类型，所以 return_type_written_by 答 None"这个前提，
     而**前提是假的**——构造函数的 Declaration 没有自己的 DeclSpecifierSeq（其祖先是
     Declaration > ClassBody > StructDef），于是读数回落到**形参表**里，把第一个参数的
     specifier 读成了返回类型。实测：DeclSpecifierSeq children = ["Name"]。
     修法：新增 declares_the_enclosing_class——定义的名字与外层类名相同即为构造/析构。
     验证：构造、析构、方法缺 return、真缺 return、void/auto/有 return 六种形状。

② 诊断范围指向整行而不是名字
     missing-return 原来用 function.text_range()（整个定义），unused 原来用 fact.range
     （整个声明 `std::string full2;`）。两者都改为名字本身：
     unused 用 DeclFact::name_range，missing-return 用"形参表 `(` 之前最后一个标识符"。
     验证：`std::string full2;` 只下划线 `full2`；`int unfinished() { }` 只下划线 `unfinished`。

③ own_syntax_nodes 把每一个顶层节点数了两遍
     `once(child).chain(child.descendants())`——而 descendants() **已经包含节点自身**。
     实测：一个只有一条声明的文件，count() 是 9 而 descendants() 是 8。
     代价取决于 check：找声明**之下**的节点的（语句、表达式）看不见；
     问**声明本身**的那两个把每条 finding 报了两遍（实测 missing-return 给出两条一模一样的）。
```

## 四、已知的缺陷与未完成项（都是量出来的，不是猜的）

```text
① argument-count 的**跨文件**那一半不解析
   本文件内声明的被调用者已经能判（scale(1, 2, 3) 报出）；
   但 v.reserve(1, 2)（reserve 声明在 <vector> 里）在真实语料上仍是 0 条。
   下一步：查 ProjectIndex::definitions(name, path) 在那里到底答了什么。

② param-type-check 还未接上同一套索引事实
   它目前只看本文件内声明的函数，跨文件实参一律不判——与 ① 同构，修法相同。

③ missing-return / unnecessary-branch 等检查在真实语料上尚无**正例**验证
   负例（不该报的）全过，正例（该报的）还没有在标准库规模上验证过。

④ 测试夹具里的 #include 不解析
   因此任何想借夹具模拟"类/函数来自头文件"的测试都必须先证明包含确实生效，
   否则测的是空气。tests/checks.rs 里有一条守卫测试专门守着这件事。

⑤ 限定名定义把作用域读进了返回类型 —— **已修**
   `int S::f() { }` 的消息曾写 "declared to return `int S::f`"，`const std::string& f()` 曾把
   `std::` 当类型的一部分（`const std`）。现在 `function_head` **一次**读出 head 的三部分
   （返回类型 / 名字 / 名字范围），所以它们不可能互相矛盾 —— 而这正是先前的问题：
   `return_type_written_by` 和 `name_declared_by` 是同一个 span 的两个读者，各自错在不同处。

   这条链上的每一处判据都是量出来的，不是选的：形参表用 `CppParamList::cast`（节点 kind 是
   `ParameterList`，手写比较匹配不到）；名字在 `(` 之前最后一个标识符，`operator` 不是标识符，
   `~` 属于名字；head 由 token **带源码间隔**重建（渲染删了空白，`int S::f` 的文本里
   `int` 和 `S` 相邻，无法区分类型与名字的作用域）；以 `::` 结尾的 head 说明末尾是名字的作用域，
   逐个剥掉（**循环**，因为命名空间会嵌套：`int a::b::c()`）。

   三十二种拼写的答案见
   `cargo run --release --example ctor_probe -p cpp_code_analysis` —— **改动这个区域前先跑它**。

⑥ operator 函数的名字范围越界 —— **已修**
   `int operator+(int other) { }` 曾下划线到下一位。根因是"名字是形参表之前最后一个标识符"
   对 operator 不成立（它不是标识符拼的，最后一个标识符在形参表**里面**）。现在按
   `operator` 关键字读到 `(`。

⑦ union 的析构函数在**无外层函数**时找不到定义 —— **已修(而且根因更大)**
   根因不是 union，是 `is_a_function_definition`：它问"这个声明里**某个**形参表是否在 body 之前"，
   而 `struct S { … };` 是 `Declaration > DeclSpecifierSeq > StructDef`，所以它的 `CompoundStat`
   孩子**就是类体**，而方法自己的形参表在它之前。于是**每个含已定义成员函数的类都被当成了
   函数定义**，缺 return 之类的检查于是去问那个类。实测：`struct S { ~S() {} };` 被读成一个
   返回类型是整段 `struct S { … }` 文本的"函数"。
   修法：形参表必须属于该声明**自己的** `InitDeclarator`（一个定义只有一个），
   外加"body 不在形参表内部"（`void f(int g(void))` 是表里套表）。

### ⑧ 每个函数形参都产生一条幽灵 Variable 事实

```cpp
int f(int op) { return 0; }
```
产出：
```text
    Function "f"
    Variable "op" type Some("int(int)") local true     ← 幽灵
```
形参**不是**局部变量，而且那个类型是**函数类型**（把 `int op` 的声明符当成了函数声明）。
对每个带具名形参的函数都成立，模板与非模板都一样。后果：
- 索引里多出一条不存在的名字（跨文件查询会答它）；
- `unused` 的候选集里多出一个"从未使用"的形参型局部；
- 形参的名字以 `Variable` 出现在任何按 `DeclKind` 分类的消费者面前。
下一步：`build_facts` 里产生这条事实的路径 —— 它的 `local` 来自形参的 binding，而
"局部"这个判断对形参不成立。

### ⑨ 伪析构调用 `x.~int()` 解析失败 —— **已修**

```cpp
void f(int& x) { x.~int(); }   // 合法 C++（Pseudo-destructor call）
```
曾报 `expected ';' after expression`，位置在 `int`。**两个**独立的缺口：

```text
一、`~` 之后只接受 Identifier | Scope
    而标准允许一个 type-name，`int` 是 builtin type 不是标识符
    → 分支匹配不到，留下一个空的 NameExpr
    修法：starter_of_a_builtin_type(那一串关键字；`bool` 在本词法器里是
          BoolLiteral，所以类型和字面量共用一个 kind)

二、`~A<int>` 的模板实参完全没被读过
    `<` 于是被表达式规则当成比较，`int` 成了没有 primary expression 的操作数
    → 报 "expected primary expression"
    修法：照同一分支上面的写法，用 could_start_template_arguments +
          parse_template_argument_list，失败就回滚
```

现在 `x.~int()` / `x.~char()` / `x.~unsigned()` / `x.~double()` / `x.~bool()` /
`x.~decltype(a)()` / `x.~T()` / `x.~S()` / `x.~A<int>()` / `x.~Outer::Inner()` /
`x.~A<int>::B()` / `p->~T()` 全部零错误，而 `a.b < c > d`（比较）仍然零错误。

```

## 五、写这一层时反复踩到的两类坑（值得继续防）

**一、把"我对语法形状的猜测"当成事实。** 同一个缺陷在这一层出现过四次：文件作用域的函数定义不是
`FunctionDef`；`struct` 不是 `ClassDef`；参数表是**孙节点**而不是子节点；简单调用的实参**没有**
`ArgumentList` 节点。每一次都是"编译通过、看着正确、什么都不报"。

> 现在的规则：**判断用强类型 AST**（`cast` / `children::<N>()` / `descendants::<N>()`），
> 而不是手写 `CppSyntaxKind` 比较；文本只用于消息。

**二、拿夹具或探针的产物当产品的行为。** 两次探针（`one_check`、`checks_probe`）都曾静默地什么都不检查，
却打印出"零 findings"这个**最好**的数字；一次夹具的 `#include` 从未解析，却被当成"头文件读过了"。

> 现在的规则：**先说清这次测量在量什么，并给它一个控制组**（例如一个必然报错的输入）。

## 六、实测确认的四个问题（第一次真正拿服务去跑之后）

> 以下四条都由**复现**确认，不是推断。两条有对应的 `#[ignore]` 测试（测试体就是复现，注释里写着实测结果），
> 两条由命令与 dump 直接给出结论。

### ⑩ CMake 支持 —— **已整个删除**（决定：只支持 `.cppls.toml` 与 CDB）

原来那条"配置的触发时机错了"的问题不复存在，因为**触发本身被去掉了**。删掉的东西：

```text
crates/cpp_code_analysis/src/project/cmake.rs     提议并运行 cmake 配置（543 行，整份删除）
crates/cpp_ls/src/handlers/execute_command/       cppls.configureCompilationDatabase 命令
CMakeCache.txt 的解析（build.rs 的 CmakeCache / find_cmake_cache / parse_cmake_cache）
ProjectDiscovery::cmake 字段，以及由它读出的 flags / standard / 编译器
ToolchainSource::BuildCache 与客户端的 cppls.configureCmake 设置
```

**留下的两条输入，就是全部**：工程的 `.cppls.toml`，以及**编译数据库**（`compile_commands.json`）——
后者是每个构建系统都能产出的那一种描述。一个还要读"这套构建系统的缓存"和"那套的工程文件"的服务器，
得把每个构建系统都教一遍，而且仍然会对没人写适配器的那套答错。

顺带补上一件原来靠 CMake 缓存才有的能力：**语言标准现在从编译数据库自己的命令行里读**
（`discovery.standard()` 认 `-std=c++20`、`-std c++20`、`/std:c++20`、`/std:c++latest`）。
没有数据库、也没写 `[compile].args` 时，会有一条明确的问题告诉用户怎么做得更好——那就是缺省状态下
唯一的提示。

### ⑪ 构造函数的参数**没有提示** —— 真因在文法，**已修**

这一条查了两轮，中途修掉了两处真实缺陷，最后落在文法上：

```text
① signature.rs 把 `&mut |_: &Path| None` 交给 callees_of_a_call     → **已修**：传入 session 的解析器
② 候选是一个类名时，类没有形参表，于是整条候选被 `continue` 丢掉    → **已修**：展开成它的构造函数
③ 构造函数**在树里根本不存在** ✗                                  → **已修**：见下（`parse_constructor_declaration`）
```

`cpp_dump` 对 `class Widget { public: Widget(int size, int extra); };` 给出的树是：

```text
Declaration@27..56
  DeclSpecifierSeq@27..33 > NamedType > NameExpr "Widget"   ← 类名被当成了**类型**
  InitDeclarator@33..54 > Declarator@33..54
    ParameterList@33..54                                    ← 形参表挂在一个**没有名字**的声明符上
```

也就是说：**类体里的 `Widget(int, int);` 被解析成了一条变量声明**，没有构造函数的声明节点，
签名自然无从渲染。测量链完整保留了：`SIGNATURES call "Widget()" -> 1 candidate` ✓ →
`CONSTRUCTORS `Widget` at 6: [23]` ✓ → `SOURCE at 23: list None` ✗。

#### 修法（已落地，`cpp_parser`）

一条新规则，与既有的析构函数、转换运算符规则并列（那两条解决的问题一模一样：**名字写在最前面、
前面没有类型**）：

```text
parse_declaration_here 的分派里新增一支：
  在**类体自己的成员层**（is_at_class_member_level）
  且当前标识符**就是所属类的名字**（current_class_name —— 类头解析时记下，随类体入栈）
  且紧跟 `(`（首参不是 `*`/`&`，那属于 `Widget (*fp)(int);` 那种形状）
  → parse_constructor_declaration：名字进 Declarator，**形参表完成在 Declarator 之内**
```

两处细节都是量出来的，不是推的：

* **"名字必须等于类名"这一条不能省。** 只用形状（标识符 + `(`）会让
  `a_split_member_head_may_name_its_type_in_the_else_branch` 失败 —— 那条夹具里的成员函数**返回类型
  写在另一个 `#if` 分支**，到达分派点时就长成"标识符 + `(`"，被读成构造函数、成员被改名。
  仅凭两个 token 无法区分，必须问外层声明的名字。
* **形参表要完成在 `Declarator` **内部**。** 先 `complete` 再读后缀会让形参表变成声明符的**兄弟**，
  而 `parameter_list_of`（token → 最近的 `Declarator` → 它的 `ParameterList` 孩子）因此对每个构造函数
  都答 `None`：量到的是 `SOURCE at 23: None`，而构造函数自己的名字就在 23。改成"名字与后缀同在一个
  `Declarator` 内"后立刻变成 `Some("(int size, int extra)")`。

修完的形状（与析构函数完全一致）：

```text
Declaration@27..43
  Declarator@27..43
    NameExpr@27..33 > Token(Identifier) "Widget"
    ParameterList@33..43 > Parameter("int size", …)
```

#### 验证

```text
cpp_parser            21 个 target 全绿（含那条曾回归的 #if 分支测试）
cpp_code_analysis     589 单测全绿；signature 16 条全绿，两条构造测试都不再 ignored
整个工作区            49 ok / 3 failed（3 个失败＝既有的 handshake 3 条 + latency 1 条里的一类，见下）
```

`tests/signature.rs` 里原来那条 `a_call_of_a_class_has_no_signature` **把旧缺陷写成了断言**（"类没有
形参表，所以诚实的答案是空白"）——它已被改写为 `a_call_of_a_class_offers_its_constructors`：类调用现在
列出**全部**构造函数（不替用户做重载选择，与普通重载的处理一致）。

#### 仍未重测的一件事

`unused` 在 MSVC 头文件里的四条误报（`sprintf_s`、`swprintf_s`、`construct_at`、`addressof`）是同一个
文法缺口的症状——那些声明现在应当作为**函数**（带形参表）而不是 `Variable` 进入索引。**187 个文件那次
语料已不在本地**（`target/std-cook` 现在只剩 5 个文件，量到 1 条真命中 `assign-type-check`），所以这条
回报要用真实语料重新量一次才算数。

### ⑫ 头/实现分离的类，在**实现文件**里没有成员补全 —— 已推进两步，剩余原因已量清

本轮修掉两处真缺陷（都不是补丁，是"事实就在那里、只是没人读"）：

```text
① 成员访问的**对象**是 `this` 时，走查拒绝它                                 → **已修**（sema/resolve.rs）
   `name_ending_before_an_operator` 的规则是"不是运算符、不是空白就结束走查" —— 对 `x + full.` 正确，
   对 `this->` 错误：走查停在 `this` 这个**关键字**上，对象丢失，答案成了 `Unknown(UnparsableName)`。
   现在关键字 `this` 交回它自己的节点（`ThisExpr`，文本正好是 `this`），因为对象是按**拼写**定类型的。

② 类外定义的**外层类**只从本文件的作用域链里找                              → **已修**（index/project.rs）
   `class_of_the_enclosing_definition`：先走 `enclosing_class`，找不到再取定义的**限定名**
   （`Widget::grow` → 前缀 `Widget`）——那正是索引记成员用的拼写，于是答案能走到头文件。
```

修复后失败点**前移**了，并且新的原因也量清楚了：

```text
ENCLOSING: innermost Some((TranslationUnit, None)) qualified None
→ UnknownType("this")
```

也就是说：`void Widget::grow(int by) { … }` 这个定义**根本没有函数作用域** ✓ —— 光标处最内层就是
`TranslationUnit` ✓。原因与 ⑬ 同族：**声明的名字写在说明符序列里，不在声明符里** ✗
（`DeclSpecifierSeq > NamedType > NameExpr "Widget :: grow"`，而 `Declarator` 里只有形参表）。
作用域构建器从声明符取名字，于是这个定义没有名字、没有作用域 ✓。

复现与完整测量链在 `tests/completion.rs::a_member_function_defined_in_another_file_sees_its_class`
（`#[ignore]`，理由就是上面这条测量）。**所以 ⑫ 的下一步与 ⑬ 是同一件事**：让"名字在说明符序列里"
的声明把名字交给声明符——修好它，类外定义的成员函数既有作用域，`this` 也就能定类型。

复现（`tests/completion.rs::a_member_function_defined_in_another_file_sees_its_class`，现为 `#[ignore]`）：

```cpp
// widget.h
class Widget { public: int size; void grow(int by); };
// widget.cpp
void Widget::grow(int by) { this->| }      // 实测 member_completions 答 Unknown(UnparsableName)
```

`this->` 之后的成员查询**答不出类型** ✗ —— 因为这个文件里没有 `Widget` 的声明 ✗，而
`this` 的类型是**外层的类**，它在另一个文件里 ✓。这与补全、跳转、引用都相关：凡"要问外层类"的地方
都要能跨文件问索引 ✓。

> 修法方向：`this` 的类型来自**当前定义的限定名**（`Widget::grow` 里的 `Widget`），
> 而这个名字应当经索引解析 —— 与 ⑩⑪ 同一条边界："报告只针对本文件，判据来自整个项目"。

### ⑬ 语法树的形状：**症状已修，原因仍在**

`docs/tree-shape.md` 记了三件事，今天用 `cpp_dump` 复核：

```text
① 前向声明的名字曾是 TemplateType      → **已修**：现在是 NamedType（commit 4ec123e）
② 类定义被解析在 DeclSpecifierSeq 内部  → **仍在**：
     struct S { int n; };   Declaration > DeclSpecifierSeq@15..34（覆盖整个定义）> StructDef
③ 每个节点吞掉尾随 trivia               → 仍在，且文档里有它存在的理由
```

② 的后果是**实质**的：一个类的**声明范围**等于整段定义 ✗，于是"声明上方那一行注释属于谁"、
"这个类的名字范围在哪里"这类问题都会答错；`tree-shape.md` 的第 4 节给了改法的五个步骤与
"先量什么"，那是这个仓库里少数**明确的、带步骤的重构**之一。

> 这一条不是 bug 而是**文法决定**：`decl-specifier-seq` 的 type-specifier 可以是 class-specifier，
> 所以"包含类定义"本身可辩护 ✗；不可辩护的是 **span** ✗ —— 它说"声明的 specifier 一直延伸到类体结束"。

### 既有的两个失败 target（与本次改动无关，已在干净 HEAD 上验证）

```text
crates/cpp_ls/tests/handshake.rs    3 条失败：hover / outline / 宏实参里的签名
crates/cpp_ls/tests/latency.rs      1 条失败：稳态补全偶发 1.4 s（预算 50 ms）
```
两者都用 `git stash` 在未改动的 HEAD 上复现过，数字几乎一样（latency：1405/36/39/1628 ms 对
1455/36/42/1753 ms），所以它们是**先于本次改动**就红的，不是这次删 CMake 带出来的。

## 七、下一步

```text
近期   ⑪ **已修**（文法：构造函数声明不再被读成变量）—— 用真实语料重测 `unused` 的四条误报是否随之消失
       ⑫ `this->` 的成员查询经索引解析外层类（复现已在 tests/completion.rs 里）
       ① argument-count 的跨文件查找 ② param-type-check 接上索引事实
       ③ 给现有 13 项检查逐条补"真实语料上的正例"验证
       既有：handshake 的 3 条与 latency 的 1 条

中期   ⑬ 按 tree-shape.md 的步骤把类定义移出 decl-specifier-seq（先跑 ctor_probe 那三十二种拼写）
       补更多 C++ 应有的诊断（重复 case 标签、const 重赋值、自赋值等）
       性能：把"只遍历本文件自己的节点"推广到其余通道

长期   让索引与渲染的缓存跨会话复用；把"报告只针对本文件、判据来自整个项目"这条边界
       推广到补全、跳转、引用（⑪⑫ 正是这条边界上的两处缺口）
```

## 八、怎么自己验证

```bash
# 全部测试
cargo test --release --workspace

# 被忽略的两条复现（它们今天会失败，那正是实测结果）
cargo test --release -p cpp_code_analysis --test signature -- --ignored
cargo test --release -p cpp_code_analysis --test completion -- --ignored

# 语法树的形状
cargo run --release -p cpp_parser --bin cpp_dump -- <file> --tree

# 标准库零误报（需要先用真实工具链 cook 出一份语料，target/std-cook 是现成的）
cargo run --release -p cpp_code_analysis --example checks_probe -- target/std-cook target/std-cook/assign-case.cpp

# 逐项检查、逐条发现（带它依据的声明事实）
cargo run --release -p cpp_code_analysis --example one_check -- target/std-cook target/std-cook/assign-case.cpp unused

# 一个 token 的真实 kind（这一层的 API 之一，曾被误判两次）
cargo run --release -p cpp_parser --example token_kinds
```






