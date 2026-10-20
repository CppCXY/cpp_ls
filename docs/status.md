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

② param-type-check 的**成员调用**已接上索引事实（含模板实例化，见 §十二 ⑱），**自由函数**还没有
   成员路径：`members_of` 给出实例化后的形参，重载集按"全部候选都不同意才报"判。
   仍缺的两半：跨文件的自由函数（与 ① 同构）；具名形参 `void f(int width)` ——
   `parameter_spellings_of` 交回的是 `int width`，`parse_type_spelling` 读不了，
   `tests/checks.rs::a_named_parameter_is_checked_too` 仍 `#[ignore]`，改之前要先做一次普查
   （参数名剥掉会让这条检查开始判它从没判过的代码；`sema::overload::parameters_in` 已经会剥）。

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

```text
⑭ **限定名定义把整个函数体丢掉** —— 详见 §十一（2026，真实工程上复现）
   渲染里没有类的定义时，`LuaSource::IsEmptyLine` 这类声明的**名字不绑、作用域不开、函数体不遍历**，
   于是函数内的局部变量既无补全也无 hover。根因是 `sema/scopes.rs:1125` 的一个 `return`；
   正确修法（不绑名字但仍遍历函数体）与验收判据都写在 §十一 里。
   **未修**：它是结构性的，影响每一个「类定义不可见」的文件，即所有 `.cpp`。

⑮ **诊断的 pull 会抽干整个索引队列**（2026，真实工程上量到）
   `prepare_for_a_diagnostic` 曾无条件做 `load` + `add_project_files` + `index_everything`，
   而 `index_everything` 循环到队列为空 —— 服务器自己的预算告警记下了它的代价：

     20:22:22  index slice: prepared 128, committed 32, write lock held 563 ms, 1258 left
     20:24:05  `prepare a file a pull request named` held the analysis for 102700 ms
     20:24:09  index slice: prepared 1, committed 1, write lock held 0 ms, 1 to cook, 0 left

   泵离完成还差 1258 个文件，那一次 pull 把它们一次性抽干 —— **102 秒的 write hold**，
   期间每一次按键、hover、补全都在等这把锁。**这才是「补全很慢」的真正原因**
   （补全查询本身实测 1.9 ms）。
   `add_project_files` 的重复入队已修（只对索引从没听说过的文件才发现）；
   **drain 的上界一度加上又被回滚，因此这条仍未修**。修法是用有界的 `advance(N)` 而不是
   `index_everything`，N 要大于一条普通 include 链。
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
近期   ⑭ **限定名定义丢弃函数体**（§十一）—— 改动点在 `sema/scopes.rs:1125` 一处，
       验收必须用真实 `.cpp`（夹具里类定义和函数定义在同一个文件，走的是另一条路）
       ⑮ **pull 的 drain 要有上界**（§四）—— `advance(N)` 取代 `index_everything`，
       N > 一条普通 include 链；这是「补全很慢」的真正原因
       **启动时间**（`docs/cold-start-cpu.md` 最后两节，已从 46.6 s 降到 9.1 s）：下一个杠杆是
       每次启动都要重建的那 47 个文件（它们 include 了找不到的头，而"记录了一次失败搜索"的 summary
       不能落盘），以及"一个文件永远渲染不出来却被每一轮 idle drain 重新排队"那个循环
       （实测 20 秒内 9 987 次 `did not render`，`pending 0 / cooking 0`）
       ⑫ `this->` 的成员查询经索引解析外层类（复现已在 tests/completion.rs 里）
       ① argument-count 的跨文件查找 ② param-type-check 的**跨文件自由函数**与**具名形参**
       ② 的兄弟：`symbol_references` 里"某条**使用**属于哪个实体"仍是词法的（§十二 ⑰）——
       声明的归属已经修了，使用还没有；判它需要每个候选文件的作用域，改动前先按
       examples/symbol_references_cost.rs 的方法量
       ③ 给现有 13 项检查逐条补"真实语料上的正例"验证
       既有：handshake 的 5 条 `#[ignore]`（4 条是手工探针，1 条要真实标准库闭包）
       与 latency 的 1 条 —— `a_completion_asked_while_the_index_fills…` 的 50 ms 稳态预算：
       同一份代码连跑三次是 通过/失败/通过（#10 分别 36 / 52 / 36 ms，单次 #0 就要 450-530 ms），
       是机器噪声而不是回归，但它确实会红，别把它当成"这次改坏了"

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

# 启动时间的分解：read / insert / drain / cook 四段（`docs/cold-start-cpu.md` 最后一节）
cargo run --release -p cpp_code_analysis --example startup_probe -- <工程根> <工程里一个真实文件>
# 加上 CPPLS_TRACE_COOKING=1 会逐次打印 drain 的形状（eager / sources / candidates / bodied）

# 逐项检查、逐条发现（带它依据的声明事实）
cargo run --release -p cpp_code_analysis --example one_check -- target/std-cook target/std-cook/assign-case.cpp unused

# 一个 token 的真实 kind（这一层的 API 之一，曾被误判两次）
cargo run --release -p cpp_parser --example token_kinds

# 三条定向回归（§十二）：documentLink 的区间、裸名字不进类、泛型成员调用的参数检查
cargo test --release -p cpp_ls --lib -- document_link
cargo test --release -p cpp_code_analysis --lib -- a_bare_name_does_not_reach a_reference_target \
    another_entitys_declaration
cargo test --release -p cpp_code_analysis --test checks -- member_argument overload_that

# 诊断 pull 的 write hold 与索引进度（§四 ⑮）—— 现在 `index slice` 是 info 级，
# 日志里能看到「prepared / committed / write lock held / 还剩多少」
#   %LOCALAPPDATA%\cpp_ls\logs\<工程名>.log
# 关键一行是 `prepare a file a pull request named held the analysis for N ms`
```
## 九、加载为什么慢 —— 量出来的结论（2024，`E:\EmmyLuaCodeStyle`）

用工程自己的 CMake 生成 CDB 之后再加载，`CPPLS_TRACE_DIAGNOSTICS=1` 给出的分阶段表：

```text
session opened in 215 ms                                    ✓ 打开本身很快
stages account for 81412 ms of the 81364 ms of indexing+cooking
[indexing]      4580.4     5.6%
parse             13.9     0.0%
index-insert       0.2     0.0%
walk           65001.7    79.8%   ← 就是这里
closure         1656.1     2.0%
```

`walk` 是 `TranslationUnit::walk` —— 构建一个文件的翻译单元时间线。它慢**不是因为缓存没做**（
`MacroDefinitions` 已经是 session 级、按 `(文件, 偏移)` 键、每定义只解析一次），而是因为**重复**：

```text
walks: 1388               索引里每个文件都走一次自己的单元
file-walks: 83,697
guard evaluations: 10,792,034      ← 1079 万次条件求值，65 秒 ÷ 这个数 ≈ 每次 6 µs
最大的闭包：~370 个文件 / ~15,000 个守卫，被 ~296 次 walk 反复走到
            （101-1000 文件这一档：296 次 walk、60,652 file-walks ＝ 全部 file-walks 的 72%）
```

也就是说：**同一批标准库头文件、同一个宏环境，被几百个源文件各走了一遍**，其中 90% 以上是重算。
（另一次用工程里手写的 `.cppls.toml` ＋ MinGW 标准库的测量是 **301 秒 / 1387 个文件** —— 闭包规模是第二个乘数。）

### 三个修法方向（按收益/风险排序）

```text
① 并行     1388 次 walk 彼此独立，是最容易拿到的近线性收益（stages.rs 的文档里本就预留了线程池的写法）
② 共享     闭包与宏环境相同的单元，头部文件的帧是同一条时间线，只有根帧不同 —— 理论上最大的收益，改动最深
③ 便宜化   每次守卫求值 6 µs 偏高；同一 (文件, 偏移, 环境) 的结果可以缓存
```

### 同时量到的两件事（都不是性能问题，但都影响它）

* **CDB 的响应文件**：cmake 生成的 108 条命令**每一条**都把 `-I` 写在 `@CMakeFiles/<target>.dir/includes_CXX.rsp`
  里；不展开就等于"零个 include 路径"。**已修**（`expand_response_files` ＋ 按文件的工具链路径 ＋ 3 条测试）。
* **cooked 事实没落索引**：cook 完 `<string>`（1004 条声明）之后 `index.definition("std::string") = None`，
  `std` 下 0 条声明。这既让跨文件问题答不出来，也很可能是重复劳动的成因之一。

### 方法上的一处自纠

我第一次的计时子探针把 `Drop` 守卫写在了 `if` 块**里面**，于是每个单元都报 0 ms —— 分文件计数是可信的，
那些毫秒数不是。凡"这次测得准不准"的问题，都要先给测量本身一个对照。
## 十、共享：先学 clangd，再试，然后发现自己做错了粒度（2024）

用户指出"共享才是正道，去看看 clangd/CLion/VS 怎么做"——这是对的。clangd 的做法（[`clangd/Preamble.h`](https://clang.llvm.org/extra/doxygen/Preamble_8h_source.html)）：

```text
"The vast majority of code in a typical translation unit is in the headers included at the top of the file.
 The preamble optimization says that we can parse this code once, and reuse the result multiple times.
 The preamble is invalidated by changes to the code in the preamble region, to the compile command,
 or to files on disk."
```

三件套：**preamble bounds**（主文件里第一个声明之前的那一段 ＝ 全部头文件）、
**`isPreambleCompatible`**（preamble 字节 ＋ 编译命令 ＋ 每个被包含文件的磁盘状态）、
**`PreamblePatch`**（只改了宏/包含时注入补丁，而不是重建整个 preamble）。
CLion/VS 是同一思路：头文件的 AST 索引一次，每个 TU 只重放宏状态。

**关键在粒度**：clangd 复用的是**整整一段（preamble）**，不是单个条件。

### 第一次尝试：做在了"每次条件求值"这一层 —— 实测是退步，已回退

想法：条件求值是 `(哪个 #if, 进入时的宏状态)` 的纯函数，于是把结果按
`(文件, 偏移, 定义集合的摘要)` 缓存进 session 级的 `MacroDefinitions`。同一工程实测：

| | wall | stages 合计 | walk |
|---|---|---|---|
| 改动前 | 81 s | 81.4 s | 65.0 s |
| 改动后 | **138 s** ✗ | **191.2 s** ✗ | —— |

**为什么错**：

* 键里 `path.to_path_buf()` **每次查询分配一次** —— 1080 万次求值就是 1080 万次分配；
* 摘要要**每遇一个 `#define` 维护一次**（SipHash），而它**每次都变** → 命中率低、开销却是恒定的；
* 于是"共享"变成了纯开销。用 `git checkout` 整文件回退，回到 51 绿 / 2 既有失败。

### 修正后的设计（下一步要做的，仍是共享，只是粒度对了）

```text
按 clangd 的粒度共享：一个**文件的帧子树**，而不是一次条件求值
  键： (文件, 进入时的宏环境摘要)          —— 与 clangd 的 (preamble 字节, 编译命令, 磁盘状态) 同构
  值： 该文件在这次环境下产生的 timeline 子树（事件 + 帧）
  命中：把子树**重新挂到新父帧上**（帧是扁平的 Vec，父索引与 from_in_parent 偏移可平移）
  失效： MacroDefinitions::forget(path) 已经按路径失效，键与值一起丢
```

同一条 clangd 规则我们已经有了一半：`MacroDefinitions`（定义解析一次，session 级、按路径失效）
就是"preamble 里的宏状态"那一半；缺的是**帧子树的复用**那一半。
另外 clangd 把"磁盘状态"也算进兼容性，对应我们的 `forget(path)` ＋ 每文件 context —— 这一点我们已经是对的。

### 方法上的教训（第二次同类的错）

上一次是"计时探针的 Drop 守卫写在 if 里"；这一次是"把缓存做在了会变的东西上"。
两次都说明同一件事：**先量命中率，再谈优化** —— 缓存键会因为每一次 `#define` 而变化这件事，
在写代码时看一眼就该发现。

## 十一、限定名定义把整个函数体丢掉 —— 一个 `return` 造成的三个症状（2026，`EmmyLuaCodeStyle`）

> 这一条是**未修**的：根因、证据链和正确修法的形状都已量清，改动点只有一处，但它是结构性的
> （影响每一个"类的定义不可见时被分析的文件"，也就是所有 `.cpp`），所以单独记在这里。

### 症状（都来自真实工程，不是夹具）

在 `LuaParser/src/File/LuaSource.cpp` 的 `LuaSource::IsEmptyLine` 里输入 `l`：

```text
补全只给出关键字:  ["class", "dowhile", "else", "ifelse", "include", "template", "while",
                    "long", "thread_local", "alignas", "alignof", "bool", …]
函数自己的 line / lineStartOffset / nextLineStartOffset 一个都没有
```

同时整个工程的加载/补全**都很慢** —— 那是**另一条独立的缺陷**（§四 ⑮：pull 的 drain 抽干整个索引队列，
102 秒的 write hold），不是这一条造成的：这一条只影响"补全给了什么"，不影响"多久才给"。

### 证据链（每一步都有测量）

```text
① 补全查询本身 = 1.9 ms                      ← 不是查询慢，是别的东西在占锁
② 渲染 5111 字节（文件 6509），1136 个节点，0 个语法错误
③ 渲染的顶层是 12 个 Declaration，全部是限定定义:
     std::shared_ptr<LuaSource> LuaSource::From(std::string &&source)
     LuaSource::LuaSource(std::string &&fileText) : _source(fileText), …
     std::size_t LuaSource::GetLine(std::size_t offset) const { … }
     …
   渲染里 contains "IsEmptyLine" = true
   渲染里 contains "class LuaSource" = **false**      ← 类的定义不在渲染里
④ 作用域树: **只有 1 个 scope —— TranslationUnit，bindings = []**
⑤ 所以 scope_at(光标) 落到 TranslationUnit → 可见名字集合为空 → 只剩关键字
```

### 根因：`declaration_parts` 里的一个 `return`

`crates/cpp_code_analysis/src/sema/scopes.rs:1125`：

```rust
if declaration_is_qualified(node) {
    if !self.names_a_scope_this_file_has(node) {   // LuaSource::IsEmptyLine → LuaSource 找不到
        return;                                     // ← 名字不绑、作用域不开、**函数体完全不遍历**
    }
}
```

**设计意图是对的**（`::` 限定的声明不该把名字绑到文件作用域，否则一个不能非限定使用的名字会和
每一个同名者冲突 —— 那条注释和它的测试 `a_qualified_declaration_binds_nothing_here` 都成立）。
**错的是把"不绑这个名字"扩大成了"整个声明都不看"**：

```text
丢掉的不只是那个名字,而是: 函数作用域 + 全部形参 + 全部局部变量
→ "补全没有函数内变量" 和 "补全只有关键字" 是同一个原因的两个面
```

### 反证：类"找不到"的两条路都通向同一个 `None`

把 `class LuaSource {};` 加进渲染，看能否救活：

```text
scope 0 = TranslationUnit  bindings=["LuaSource:Class"]   ← 绑定有了
scope_with_qualified_name("LuaSource") = None             ← 但作用域还是没有
scopes.len() 仍然是 1
```

因为 `class_like` 里另有一道守卫（`scopes.rs:707`）：

```rust
if body.children().next().is_none() { return; }   // "空 body 不开作用域"
```

两条路都让 `scope_of_a_qualifier` 返回 `None`：

```text
真实渲染: 没有 class LuaSource             → 找不到
我加的:   class LuaSource {}; 是空 body    → 守卫拒绝建作用域 → 找不到
```

### 正确修法的形状（未实现）

```text
在限定名解析不到时: **名字不绑,但仍然遍历函数体**
  → 形参 + 局部变量绑进一个作用域(父 = 该声明所在的 scope)
  → 函数内补全、hover 恢复
  → "名字不绑" 这一点保持不变(a_qualified_declaration_binds_nothing_here 仍成立)
```

验收判据（**必须用真实文件，不能用夹具**）：对 `LuaSource.cpp` 在 `IsEmptyLine` 内问补全，
`line` / `lineStartOffset` / `nextLineStartOffset` 必须出现。

### 方法上的教训（第三次同类）

§五 的第二条说过"别拿夹具当产品"，这一次是它的一个更隐蔽的变体：

> **夹具里 `class LuaSource` 是我自己写的，所以合成夹具永远测不到"类的定义不可见"这个真实条件。**

`docs` 里那些 `struct Widget { … }; void Widget::g() { … }` 的夹具**全部**把类定义和函数定义放在
同一个文件里 —— 于是它们全部走的是"限定名解析成功"那条路，而真实工程里 `.cpp` 的常态正相反：
类在头文件里，渲染里根本没有它。**这一类缺陷只能被真实文件暴露。**

### 复现命令

```bash
# 真实文件上的补全（把 nextLineStartOffset = 0; 改成 = l;，光标在 l 之后）
cargo test --release -p cpp_code_analysis --test completion -- --ignored

# 渲染与文件的第一处差异、渲染的顶层节点、作用域数量
cargo run --release -p cpp_parser --bin cpp_dump -- <file> --tree
```

## 十二、三个"看起来是设计问题、其实是同一把键用错了"的缺陷（2026，`test_cpp`）

三条都是用户在一次会话里报出来的，三条都**已修**，而且三条的根因互不相同 —— 放在同一节是因为它们
有一个共同形状：**某个查询用"字符串拼写"当键，而语言用的是别的东西。**

```text
documentLink     键 = 整条指令        语言要的是"路径"那一段
跳转/引用        键 = 名字拼写        语言要的是"名字 + 所在作用域"
param-type-check 键 = 类里写的形参    语言要的是"实例化之后的形参"
```

三条都不是猜出来的：每一条都先用真实文件（或按真实文件形状搭的多文件夹具）量出"它现在答什么"，
再改那一处。

### ⑯ documentLink 覆盖整行，而不是路径 —— **已修**

实测（真实 `main.cpp`）：每一行 `#include` 的下划线都从 `#` 一直画到行尾注释之后。

```text
根因  IncludeFact::range 是**整条指令**的 range（含行尾 trivia）
      它不是 bug —— 它是"这条指令在哪"的答案（Session::header_at 拿它把光标匹配到指令）
      但 link 是另一种形状：可点击区间，应该是文件名本身。两者共用一个字段，于是 link 用了指令的答案
修法  sema::resolve::header_name_in(root, directive) —— 从**文件自己的**语法树里读出 HeaderName
      token，再剥掉 <> 或 ""；document_link 改用 view_of_the_file（渲染里没有指令）
     None 时回退到整条指令（#include HEADER 这种宏包含没有 HeaderName token）
验收  a_resolved_include_becomes_a_link_that_covers_the_path（列 10..18，注释在 20）
      a_directive_with_no_name_in_it_falls_back_to_its_own_range
```

### ⑰ 裸名字被解析到了类成员 —— **已修**

实测（`test.h` + `main.cpp`，真实文件）：在 `main.cpp` 里对 `printf` 求定义，peek 列表给出**三条**：

```text
test.h:21   void printf(int i);              ← Name 的成员
test.h:26   void Name::printf(int i) { }     ← 同上，类外定义
stdio.h     __CRTDECL printf(...)            ← 唯一一条 main.cpp 真能写的
```

```text
根因  ProjectIndex 把事实按 **DeclFact::name** 建索引，于是"所有头文件里叫 printf 的东西"全是候选
      而 C++ 按 **名字 + 所在作用域** 解析：文件作用域里的裸 printf 只能指文件作用域的 printf
修法  ProjectIndex::definitions_in_scope(name, visible_from)
      · 裸拼写：scope 是**类**的事实一律丢掉（scope 的判定问 index 里那条声明本身是不是 Type，
        且比的是 qualified_name == scope，避免把 Outermost 里的同名嵌套类当成 Outermost 是类）
      · 限定拼写（Name::printf）：不动，那本来就是"从外面点名一个成员"的写法
      · 索引里查不到这个 scope 是不是类 → 保留（"我看不出来"不能变成"没有"）
      调用点在 definitions_across_files 的 NotDeclaredHere 分支，且**两种情况例外**：
      (a) 光标在成员访问上（x.eof() 的对象类型未知时仍要按名字找）
      (b) 光标在 void C::g() { … } 的函数体里 —— 这里裸名字**可以**是 C 的成员，
          而 C 的定义在别的文件里。判据是 inside_a_definition_written_with_a_qualifier
          （形参表 ( 之前那个标识符带 ::）。这条是**保守**的：漏判只是回到旧行为，不会造出新答案
验收  a_bare_name_does_not_reach_a_class_member（三条断言：1 条 / 不过滤时 3 条 / 限定名 2 条）
```

**同一把键的第二个后果，两半里修了一半**：`symbol_references` 收引用时是词法的 —— "每个等于末段拼写的
identifier"，几十万 token 里当然会把同名不同物的东西收进来。

```text
已修  某条**声明**若属于另一个实体，就不是这条符号的引用
      index 知道每个声明名字的偏移和它的 qualified name，于是 Name::printf 的声明与类外定义
      从 printf 的引用列表里消失（此前重命名会把它们一起改掉 —— 这是会改坏代码的那种错）
      同一 qualified name 的重复声明仍然保留（extern int n; 写在两个头文件里）
未修  一条**使用**属于哪个实体：printf("x") 在 Name 的成员函数体里指的是 Name::printf，
      在全局函数里指的是 ::printf。判这个需要**候选文件各自的作用域**（每文件一次解析），
      而候选集是"所有能看见该声明的文件"（标准库规模下几百个头文件）
      取舍写在 index/references.rs 的模块文档里，改动前先按 examples/symbol_references_cost.rs 量
验收  another_entitys_declaration_of_the_same_spelling_is_not_a_reference
```

### ⑱ 泛型成员调用的参数检查不生效 —— **已修（三处叠加）**

实测（`main.cpp` 第 20 行，用户报的那一行）：

```cpp
std::vector<int> anotherVec;
anotherVec.push_back("123131");     // 期望 param-type-check，实际 0 条
```

沿路量出来的是**三个**独立的缺口，每一个单独存在都足以让这一条静默：

```text
① 替换读的是渲染，偏移是文件的
   substituted_members 用 view.source（渲染）配 fact.range（文件坐标）去读 template<...>
   头文件里 class 之上任何一个 #include 都会让偏移错位 → 找不到模板头 → T 还是 T
   实测：夹具里 class 上方加一行 #include，Box<int>::push_back 就答 (const T& value)
   修法：view.written_text().unwrap_or(&view.source)

② 两个候选 = "什么也不说"
   MSVC 的 vector 写两遍 push_back(const _Ty&) / push_back(_Ty&&)，于是 member_parameters 返回 None
   修法：member_parameter_lists 返回**全部**候选，判据改成检查层一直文档化的那条 ——
        **每一个**候选都不同意才报；有一个 Yes 或 Unknown 就沉默

③ 引用类型的目标在转换关系里没有分支
   const int& 既不是 int 也不是指针，convertible_to 落到 _ => Unknown，而检查只报确定的 No
   修法：Type::convertible_to 先看穿目标顶层的 Reference（绑定就是"能不能转成 referent"）
        并给了一条单测：const char[7] → const int& 是 No，int → const int& 是 Yes，
        const char[7] → const std::string& 仍是 Unknown（类可能有转换构造函数）
验收  a_member_argument_is_checked_through_a_header_and_against_every_overload
      an_overload_that_takes_the_argument_keeps_the_check_silent
      a_reference_target_is_judged_by_its_referent
```

### 方法上的教训（第四次同类）

> **单文件夹具测不到"头文件里的类"这条真实条件。**

⑱① 是这一节最有价值的一条：`tests/checks.rs` 里那条 `a_member_parameter_is_checked_after_instantiation`
**一直是绿的**，因为它的夹具把 `namespace std { class vector … }` 和调用写在同一个文件里 ——
渲染与文件坐标相同，于是①那条缺陷不存在。一旦 class 在头文件里（`#include` 之上还有别的指令），
坐标错位，替换静默失效，而测试没有任何一条变红。

修法的验收因此改成**多文件夹具**（`codes_of_files`）：类在 `box.h`，且 `box.h` 里 class 之上有一行
`#include`，调用在 `a.cpp`。这三件事同时在场，才复现用户那一行。


## 十三、真实 C++23 模块工程上的四个缺陷（2026，`C:\Users\zc\Desktop\test_module`）

用户的抱怨是一句话："语言服务在这上面的表现并不好，首先他就几乎不认识模块，然后在有模块的文件内几乎就不能补全，
hover 需要等待的时间也非常长。" 四项，三项已修，第四项已量清、待做。

工程：CMake + Ninja + MinGW GCC 15.1，`-std=c++23 -fmodules-ts`，5 个源文件（`math.cppm` / `shapes.cppm`
接口单元，`math.cpp` 实现单元，`main.cpp` 导入 `math`/`shapes`，`std_module_demo.cpp` 导入 `std`），
`build/compile_commands.json` 4 条命令。

### ① `compile_commands.json` 里的 Windows 路径被吃掉反斜杠 —— **已修**

`split_command_line` 把 `\` 一律当 POSIX 转义字符，于是：

```text
真正    C:\Users\zc\Desktop\mingw\mingw64\bin\c++.exe
解析成  C:UserszcDesktopmingwmingw64binc++.exe
```

discovery 没法运行这个编译器，**静默退回机器上的另一个工具链**：

```text
              改前                          改后
dialect       Msvc                          Gnu
include       8 条 MSVC / Windows SDK       6 条 MinGW GCC 15.1
索引文件数     304                           478
```

一个 MinGW 工程被当成 MSVC 分析，`#include <print>` 落到错误的库或落空 —— 这就是"不认识这个工程"的一半。
修法：`\` 只在**引号或空白**前才是转义，其余一律是路径分隔符；`C:\Users\…`、`"C:\Program Files\…"`、
`\\server\share` 全部保住，`a\ b` 与 `\"` 照旧。测试 `a_windows_path_keeps_its_separators`。

### ② 请求路径给模块解析传了一个**文件**，而它要的是**目录** —— **已修**

`ModuleScanner::resolve_module` 把模块名拼到"给定目录"上，而 `Session::module_interface_units_of` 传的是
**导入它的那个文件**，于是候选成了 `<dir>/main.cpp/math.cppm` —— 没有任何文件系统有这条路径：

```text
resolve_module("math")            no file in this project declares module `math`
read_the_modules_a_file_imports   0
imports_a_module_that_is_not_read 永远 true   → 模块文件里**每个请求**都拿一次写锁
```

实测修复前后：`read_the_modules` **0 → 2**，`interface_unit_of("math")` **None → `…/src/math.cppm`**。
标准库模块的测试一直是绿的，因为它们的 `.ixx` 是从 include path 旁边的 `modules/` 找到的，坏掉的第一个候选
从来不是回答的那个；新测试的夹具**故意不配 include path**（`a_module_beside_the_importing_file_is_read_in`）。

### ③ `std::` 补全为空 —— 宏把预算挤满了 —— **已修**

`name_completions_at` 把文件自身及其直接包含头文件的宏追加到**每一个**名字列表，包括 `::` 限定位置。
空前缀时排序落到 label，而宏全是大写/下划线，排在所有小写名字之前：

```text
`std::`     200 条，第一条 NOATOM … _WINDOWS_ … string/expected 全被截掉
`math::`     44 条，同样是宏 + 少量成员
```

而 `declarations_in("std")` 有 **2352** 条 —— 数据一直在，是列表被宏占满了。修法：宏只追加给普通名字位置，
`::` 限定位置不追加（宏不是任何东西的成员）。改后：`std::` → 200 条真名，`math::` → 模块导出的 5 个，
`std::string` → `string, string_view, stringbuf, …`。

### ④ 请求里做全量索引 —— **已量清，待做**

`read_the_modules_a_file_imports` 内部调用 `index_everything()`：模块文件里第一次 hover/补全要等整条索引链。

```text
read_the_modules  2 module(s)  1416 ms（热）/ 8906 ms（冷）      紧接着 index_everything 57 ms
```

### 顺带做的三件模块功能

```text
高亮       module/import/export 关键字 + 模块名(namespace)；`int import = 1;` 仍是变量
documentLink  `import math;` → src/math.cppm，区间是**模块名**而不是整条声明
补全       `import ` → math, shapes；`import sh` → shapes（索引 + 工程里的 .cppm/.ixx，一行一个名字）
```

### ⑤ `export namespace math` 的名字不是 exported —— 导入方永远看不到 `math` —— **已修**

用户在补全里看不到 `math`（截图里 `mat|` 只有一堆 Windows SDK 名字）。索引里有这条 fact：

```text
"math"   kind Namespace  scope None  exported false      ← 而 add / Vec2 / divide 都是 true
"Number" kind Type       scope Some("math") exported false
```

原因是 **导出区域是按"声明形状"记录的**，而 `NamespaceDecl`/`ConceptDecl` 既没有 specifier 也没有
declarator，所以它们根本不成其为 shape —— 不是 shape 的声明没有地方记 `exported`，于是恒为 false。
导入方只接收 **exported** 的 fact，所以 `math` 在索引里、在导入方不可见，`math::add` 也就无从写起。

修法：`declares_a_shape` 加上 `NamespaceDecl` 和 `ConceptDecl`（它们成为 shape 的唯一目的是让自己
的**名字在路径上**）。测试 `an_exported_namespace_and_its_concept_are_exported_facts`。

### ⑥ 签名提示几乎不出现 —— 声明所在文件没有渲染就整个跳过 —— **已修**

`signature::signatures_at` 只从**声明所在文件的渲染树**里读参数表，而渲染是查询**不建**的
（一个头文件 65–190 ms），`Session::view` 于是入队并返回 `None`，那个候选就被 `continue` 掉。客户端每次
`(` 只问一次，"下次再问就有了"永远不会发生：

```text
math::add(        0 个签名（第一次问是 0，索引跑完之后再问仍是 0）
std::println(     0 → 修法后 4 个重载
```

修法：`Callee` 带上 fact 里的 `parameter_list`（索引本来就有）；渲染缺失时用它拼出 label。丢掉的是参数的
高亮区间（那是渲染坐标），保留的是签名文本、参数个数和当前参数。测试
`a_declaration_in_an_uncooked_file_still_answers` —— 它显式断言声明所在文件**没有**渲染。

### ⑦ `import std;` 在 GCC 上找不到 —— 因为它的接口单元是一个**源文件** —— **已修**

用户在 `std_module_demo.cpp` 上看到我们的诊断（"no file this project contains declares module `std`"）。
先查编译器本身：

```text
$ c++ --version                      c++.exe (x86_64-win32-seh-rev0, Built by MinGW-Builds) 15.1.0
$ c++ -std=c++23 -fmodules-ts -fsyntax-only std_module_demo.cpp
std: error: failed to read compiled module: No such file or directory
std: note: compiled module file is 'gcm.cache/std.gcm'
```

**所以编译器自己也不认识它** —— MinGW 不发预编译的 `std.gcm`，我们的诊断是对的，但只对了一半：GCC 虽然
没有 `std.gcm`，却**带着这个模块的接口单元源码**：

```text
<gcc>/include/c++/bits/std.cc         97 KB：module; #include <bits/stdc++.h> #include <execution>
                                      #include <strstream>  export module std;  export namespace std { … }
<gcc>/include/c++/bits/std.compat.cc  17 KB
```

修法：模块解析对 `std` / `std.compat` 额外尝试 `<include>/bits/<name>.cc`（只对这两个名字 —— `bits/<x>.cc`
不是约定，是这一个库把这两个文件放在那里）。之后一切照旧：它就是接口单元，读它、拿到它导出的名字。

实测（`std_module_demo.cpp`）：

```text
read_the_modules          1 module(s)   4491 ms 冷 / 532 ms 暖（缓存后）
索引里 std 作用域          4944 条声明
std::vector               1 declaration(s)
`std::vec` 补全            vector                    4 ms
`std::pr` 补全             print, println, printf, promise, projected, …   5 ms
```

冷启动那 4.5 秒是**读 `<bits/stdc++.h>` 一遍**的代价 —— 和任何 `#include <bits/stdc++.h>` 的文件一样，
之后进 summary/timeline 缓存。

**让编译器也认识它**（这样 `std_module_demo.cpp` 才能真正编译），在希望生成 `gcm.cache` 的目录里跑一次：

```text
c++ -std=c++23 -fmodules-ts -c "<gcc>/include/c++/bits/std.cc" -o std.o
```

实测：5 秒，产出 `gcm.cache/std.gcm`（30 MB），之后 `c++ -std=c++23 -fmodules-ts -fsyntax-only
std_module_demo.cpp` 退出码 0。也可以把 `gcm.cache` 放到任意目录并用 `-fprebuilt-module-path=<dir>`。

测试 `the_standard_module_is_found_in_gcc_include_tree`。

## 十四、语义高亮为什么会花 11 秒 —— 四处"每个 token 做一次"（2026）

用户指出实现里的两件事："你会先取得每个 token 的文本再判断 token 的 kind"、"我们获取文本的方式太原始了"。
两件都对，而顺着查下去又找到两件更贵的 —— 其中一件不在 encode 里，在它前面的分类里。

```text
一次 encode（236 000 字节 / 164 000 token）        2119 ms → 24 ms      （88 倍）
分类一次（<algorithm>，7 966 个标识符）           11 258 ms → 387 ms    （29 倍）
分类一次（2 000 个函数，93 KB / 54 000 token）    95 777 ms → 36 ms     （2 680 倍）
```

四处：

```text
① 先取文本再判 kind
   push_keywords 对**每个** token 先切 &str，再判断它是不是关键字；
   164 000 个 token 里关键字 8 000 个、需要文本的 32 000 个，其余 132 000 个白切。
   修法：先看 kind —— Identifier 才需要文本（requires/concept），BoolLiteral 才需要（bool vs true/false），
        其余 kind 直接由 kind_of_keyword 回答，这个函数现在**不接收文本**。

② 模块指令那一遍是"每个 token 都问祖先"
   第一版为了给 module/import 上色，遍历**全部** token、每个问三次父节点，再 to_string() ——
   一个不 import 任何东西的头文件（<algorithm>，4 100 token）做 4 100 次祖先查询来什么也不画。
   修法：先找 ModuleDecl/ImportDecl 节点（模块文件里一两条），只在这些节点内部走 token。

③ 行查找是 O(文件长度)，于是整个 encode 是 O(token × 文件长度)
   util::line_body 用 source.split_inclusive('\n').nth(line) 找行 —— 从字节 0 数起。
   而它每个 claim 被调用：push_range 两次、build 两次。24 000 个 claim × 4 次 × 236 000 字节 ≈ 5.6 GB 扫描。
   修法：line_body 收 LineIndex（get_line_offset 是二分），push_range 的"文件能否放置"改成**长度比较**。

④ 分类里每个声明名都重建一次声明形状表
   semantic::modifiers_of 为了读 const/static 调用 DeclarationShapes::of(&view.root) ——
   那是**一次整棵树的遍历**，而它按名字调用。<algorithm> 有 1 639 个声明名，于是建了 1 639 次表。
   11.3 秒里几乎全部在这里，与 encode 无关 —— 所以只量 encode 永远看不到它。
   修法：classified_names 建一个 OnceCell<DeclarationShapes>，第一个需要它的名字建表，其余共享。
```

### 方法上的教训

> **"每个 token 做 X"在 164 000 个 token 上是四个数量级的差别；而"每个 token 建一张表"更贵。**

只有①是用户看出来的。③是 encode 的主项（2119 ms 里约 2 000 ms），藏在 `position_in_file` 这个
"看起来只是查个行列"的函数里；④是分类的主项，藏在 `modifiers_of` 这个"看起来只是读个 const"的函数里。
两者形状相同：**一个 O(n) 的辅助函数被放在一个 O(n) 的循环里**，而它自己的名字听起来是 O(1) 的。

量的时候必须先**分层**（`view` 108 ms / 分类 95 777 ms / encode 24 ms），否则只会看见最后一段——
而用户报的"高亮卡"里，被卡住的是中间那一段。

### 留下的探针

```text
cargo test --release -p cpp_ls --lib -- a_big_file --ignored --nocapture
cargo run --release -p cpp_code_analysis --example semantic_probe -- <文件列表> --limit 6
```

前者打印两个夹具的 token 数、view / 分类 / encode 三段耗时；后者对真实文件打印每个文件的
标识符数、分类数、耗时与来源分布。


## 十五、一次会话里的四份报告，两份"缓存了一份错误的答案"（2026）

四条都来自同一个编辑器会话，前三条已修，第四条已定位、未修。

### ① `std::endl` 启动时不是函数，编辑一下就是 —— **已修**

这不是分类的错，是**时机和缓存**：语义高亮一次给整个文件，客户端把它缓存到下一次编辑。启动时请求到达时，
`is_locally_ready` 只问"这个文件的**直接** include 读了吗"，而 `std::endl` 在 `<ostream>` 里，经
`<iostream>` 十层才到 —— 直接层读完了，深层的还没有，于是那些名字没有分类，客户端的语法色就留在了上面。

修法两半：

```text
判定     Session::is_closure_ready —— 整个可见闭包都有摘要才算准备好（visible_files + 摘要查询）
刷新     workspace/semanticTokens/refresh —— 索引落定后让客户端重问一次（与 inlayHint/refresh 同一机制）
```

测试 `a_file_is_closure_ready_only_once_what_it_reaches_is_read` —— 它把最深那个文件的摘要丢掉，
断言 `is_locally_ready` 仍为真而 `is_closure_ready` 为假。

### ② 宏的补全行带着 `int ()` —— **已修**

`#define FULL_NAME_FORMAT "Full name: {}"` 在补全里显示 `int ()`。那个 detail 来自
`DeclFact` 的 `returns` + `parameter_list`，而宏的 fact 是**克隆**来的：

```rust
let mut declaration = summary.declarations.first()?.clone();   // ← 文件的第一个声明
declaration.name = name; declaration.kind = MacroLike;         // ← 只改了这两个字段
```

于是 `int main()` 的 `returns`、`parameter_list`、`bases`、`parameters`、`local` 全都留在了宏的 fact 上，
`completionItem/resolve` 又把它们读出来画成 `int ()`。

修法：克隆之后**清掉所有属于那个声明的字段**（`type_of`、`returns`、`bases`、`parameters`、`local`），
只留 fact 的形状。测试 `a_macro_is_not_shown_with_a_signature` 断言宏 fact 的这几项都是空的。

### ③ `printf(…)` 没有参数提示，`push_back(_Val: …)` 有 —— **已修**

同一个文件里相邻两行：成员调用的名字来自 `members_of` 的 **fact**（不需要渲染树），自由函数的名字来自
`parameter_list_of`，那需要**声明所在文件的渲染** —— 而查询不建渲染。于是系统头里没有渲染的自由函数
静默地没有提示。和上一节 ⑥（签名提示）是同一个根因，只是另一条路径。

修法：`inlay::parameter_hints` 在 `parameter_names_for` 失败时，用 `Callee::parameters`（fact 里的参数表
文本）读名字 —— **把那段文本包成一条合成声明去 parse**（`void __hint(const char* __format, ...);`），
而不是按逗号切：`int (*f)(int)`、带默认参数的 `map<int, string> m = {}`、`...` 都会切错。
测试 `a_call_into_an_uncooked_header_is_still_hinted` —— 它断言声明所在文件**没有**渲染。

### ④ `std::vector<int>::push_back("123131")` 没有类型错误 —— **已定位，未修**

检查本身对"类模板的成员"是好的：`void push_back(const _Tp& __x);` 在 `std::vector<int>` 上会报
（测试 `a_member_of_a_class_template_is_checked_after_substitution`）。

断掉的是**别名这一步**，而这正是 libstdc++ 真实写法：

```cpp
template <class _Tp> class vector {
public:
    using value_type = _Tp;
    void push_back(const value_type& __x);   // ← 参数类型是类自己的别名
};
```

`members_of` 会把类实参**替换进成员的 `type_of`/`returns`**，但 `parameter_list` 是声明的**原文**，
替换不进去 —— 于是目标类型停在 `const value_type&`，`convertible_to(const char[7], const value_type&)`
答 `Unknown`，而"无法比较"的检查必须沉默。

修法（下一步）：让 `members_of` 在替换成员类型时也替换 `parameter_list`（文本替换，`_Tp` → `int`），
并且先把类自己的别名成员（`value_type` → `_Tp`）展开一次。测试已经写好并 `#[ignore]`：
`a_member_parameter_written_with_the_classs_alias_is_checked`。

### 方法上的教训（第五次同类）

> **一个"少给一些"的加速，会被客户端缓存成"少给就是全部"。**

①和③都是"查询不建渲染/不读闭包，先用已知的回答"的产物 —— 这个决定本身是对的（否则每个请求几百毫秒），
错的是**没有人再告诉客户端可以重问了**。①现在有了 `semanticTokens/refresh`，③有 `inlayHint/refresh`；
两者都是"答案变了而文档没变"这一个协议盲区，而这类答案在这个服务器里还有几处（诊断已经是 pull）。

## 十六、`enum` 的作用域没有名字 —— 四个症状，一个根因（2026）

用户报了三件事，其中两件是同一个缺陷：

```text
① 类里写自己的名字（injected-class-name），hover 显示的是构造函数
② IsTriviallyCopyConstructible::Yes 是枚举值：没有 hover、不能跳转、也不渲染成 EnumMember
③ is_trivially_assignable_v 是变量模板，hover 却给 Ambiguous
```

### 根因：枚举的作用域创建时**没带名字**

`enum class E { Yes, No };` 在语法树里是 `DeclSpecifierSeq > BuiltinType > [EnumKeyword, ClassKeyword,
NameExpr, CompoundStat]` —— **没有 declarator**。而作用域构建用的 `declared_name` 在没有 declarator 时退化成
"节点最后一个**直接** token 里的标识符"，这里一个都没有（名字在 `NameExpr` 子节点里），于是 `named = None`：

```text
create_scope(ScopeKind::Enum, …)      ← 无名作用域
fact(Yes).scope = None                ← 于是枚举值被当成文件作用域的变量
```

后果一路串起来：`fact.qualified_name()` 是 `Yes` 而不是 `std::E::Yes`；跨文件 `E::Yes` 找不到（qualified
lookup 按作用域找）；语义高亮把它画成 `variable`(真正的变量色)；而且裸 `Yes` 在工程里到处可见。

**修法**：`bare_class_like` 改用新的 `specifier_name`（读 `NameExpr`，遇到 `CompoundStat` 就停，所以不会把
枚举值当名字），并用 `create_named_scope` —— 作用域带名字了。`enum_like`（`enum Plain { … }` 那条路）也改成
读解析器自己的 `CppEnumDef::get_name`。测试 `an_enum_holds_its_enumerators` 现在断言
`qualified_name_of(enum_scope) == Some("std::E")`（之前只断言了 shape，名字根本没被检查）。

`IndexedKind` 增加了 `enumerator` 一位（`Variable` + 有 scope + 没有 `type_of`），于是高亮把它画成
`enumMember`；索引里没有新 `DeclKind` 变体，摘要格式不变。

### 另外两个

```text
injected-class-name   bindings_named 不再把 Constructor/Destructor 当作名字查找的答案：
                      struct X { X(); } 里的 X 是**类**（C++ 的 injected-class-name），不是构造函数。
                      构造函数调用不受影响 —— callees_of_a_call 本来就需要类，再由它给出构造函数集合。
using 声明是"同义词"   C++ 原文：using 声明 "does not introduce a new declaration"。
                      definitions 在**还有真实声明**时丢掉 DeclKind::Other 的 using fact：
                      using std::E; + enum class E 不再 Ambiguous；变量模板同理。
qualifier 解析        through_an_alias 现在也会把"前缀本身就是一个实体"的情形改写为它的 qualified_name
                      （using 声明的枚举/类型），不再只跟 alias 的 target。
```

### 一个已有偏好，被测试钉住

用户说 "我更喜欢 auto、const 这种非控制流关键词用蓝色" —— 这本来就是设计：`kind_of_keyword` 把 `auto`/
`const`/`constexpr` 归为 `TypeKind::Modifier`，控制流（`return`/`for`）才是 `Keyword`。
`a_keyword_is_drawn_as_what_it_is` 现在把 `auto` 与 `const int` 的 `const` 也断言成 `Modifier`。

## 十七、`push_back(const value_type&)` —— 别名参数的两处断点（2026）

上一节 ④ 记录的 `std::vector<int>::push_back("123131")` 不报，修好了，两个断点各一处：

```text
① members_of 的替换根本跑不到（所有 check 走的都是 SemanticModel）
   它把类实参替换进成员的 type_of/returns/parameter_list，但模板参数名是从**文件文本**里读的，
   用的却是 fact 的 offset（fact 的 offset 是文件的，view.source 可能是渲染的）——
   Session::members_of 传的是 written view 所以对，SemanticModel 传的是渲染，于是 names 为空、静默跳过。
   修法：名字取自 **fact 的 parameters**（`declared_template_parameters_with` 早已记录），不需要任何文件。

② 类自己的别名没有展开
   libstdc++ 写 `void push_back(const value_type& __x);` 且 `using value_type = _Tp;`。
   只替换 `_Tp → int` 会留下 `value_type`，`convertible_to(char*, const value_type&)` 答 Unknown → 检查沉默。
   修法：替换前先把该类的别名成员展开一遍（别名目标就在它自己的 fact.type_of 里），再替换模板参数。

③ 带默认形参的模板：写出来的实参比形参少
   `std::vector<int>` 只写一个，而 `template <class _Tp, class _Alloc = …>` 有两个名字；
   `substituted_spelling` 在两边不等长时**原样返回**。修法：两边按较短的截断（位置替换本来就只覆盖写出来的那些）。
```

实测（真实 libstdc++ `<vector>`）：

```text
main.cpp:10  [param-type-check]  this passes `"123131"` where `const int&` is declared for `push_back`
```

测试：`a_member_of_a_class_template_is_checked_after_substitution`（`const _Tp&`，本来就过）与
`a_member_parameter_written_with_the_classs_alias_is_checked`（`const value_type&`，上一节被 `#[ignore]` 的那条，
现在通过）。

## 十八、`variableTemplate`：一个自定义语义类型，和六个当时不支持的扩展语法（2026）

### ① 变量模板不再是 `variable`

用户第二次提这件事（截图里 token 检查器显示 `variable`）。上一轮的答复是"LSP 没有这个类型"，这仍然成立 ——
但"没有标准类型"不等于"只能发 `variable`"：协议的 legend 是**服务器自己声明**的字符串表，自定义名字是允许的，
只是客户端不认识就画不出来。

```text
TypeKind::VariableTemplate  ->  SemanticTokenType::new("variableTemplate")
分类  声明处：缓冲区里有 template head（declared_template_parameters_of）
      索引处：fact.parameters 非空（declared_template_parameters_with 早就记录了）
兜底  builder.push_range 返回 false（客户端没有这个类型）→ 再发一次 Variable
```

兜底那一步是关键：**不发比发错更糟** —— 客户端不认识时若不兜底，这个 token 会直接消失，连语法高亮都没有了。
测试 `a_template_parameter_is_a_type_parameter_and_a_variable_template_is_a_variable_template` 同时钉住
"默认发 custom"和"只声明 `variable` 的客户端拿到 `variable`"。

要给它上色（VS Code，任选其一）：

```json
"editor.semanticTokenColors": { "variableTemplate": { "foreground": "#4FC1FF" } }
```

### ② `cpp_parser` 里六个 `#[ignore]` 全部修好，现在一个都不剩

```text
GNU     `({ int a = 1; a; })`      语句表达式：`(` 后跟 `{` 就是复句语句，读成 CompoundStat
        `a ?: b`                   elvis：`?` 后直接是 `:`，中位操作数缺省 —— 不合成任何节点，两个操作数照旧
MSVC    `__try { } __except (1) { }`  __try/__except：与 try/catch 同形，filter 是**表达式**不是形参表
        `__try { } __finally { }`      __finally：同一循环里的另一种 handler
        `__interface I : IUnknown { }` 类关键字用标识符拼写：名字 + 基类子句 + 体，就是 class 的读法
Clang   `^(int n) { … }`           块字面量：操作数位置的 `^` 只能是它，读成 LambdaExpr（无捕获的闭包）
        `void (^block)(int)`       块**声明**：`^` 就是函数指针的 `*`，在 declarator 里读成 PointerType
C++11   `for (int x : {1, 2, 3})`  花括号初始化作为 range：`{` 在表达式位置就是列表
```

其中 Clang 那半条值得记一笔：**测试的注释原本写着"块类型能解析"**，实际上不能 —— `void (^handler)(void)`
报 `expected `;` after expression`。所以修的时候两半都要做：declarator 里的 `^` 和操作数位置的 `^`。

验收：`cargo test -p cpp_parser` 全绿，且 `grep -r '#\[ignore' crates/cpp_parser` 已无结果。

### 剩下的 ignored 测试（不在 cpp_parser）

```text
cpp_code_analysis/tests/types.rs        3 条（known regression / root cause，各自记了原因）
cpp_code_analysis/tests/checks.rs       1 条（一个已解析为 neither side 的形参）
cpp_code_analysis/src/index/project.rs  1 条（一个测量型用例）
cpp_ls/tests/handshake.rs               5 条（3 条 needing the release profile / a reading, 2 条 probe）
```

## 十九、补全的三个症状，一个坐标缺陷（2026）

用户报的是三件事，其实是同一处：

```text
`auto x = a|`    列表里出现**所有**作用域里的名字(没过滤)
`auto x = an|`   列表只剩关键字(`anotherVec` 不见了)
接受 `anotherVec`  写出来是 **`aanotherVec`**
```

三个都由"**替换区间为空**"解释：服务端告诉客户端的 `replace` 是 `cursor..cursor`，客户端于是把补全项插在光标处 ——
已经打出来的 `a` 留在前面；而服务端算 `prefix` 时也读到空串(没过滤)，`an` 那次读到的是最后一个 `n`(所以只剩关键字)。

### 根因：光标在"一个 token 的末尾、文件里原文已被删掉"的位置

`auto x = a` 后面是换行,而渲染里换行不产生 token —— 于是光标(file 75)落在 span 之间的**空隙**里。
`reading_offset_of` 对空隙的既有规则是"回答**下一个** token 的起点",这里就是下一行的 `return`:渲染偏移 53。

```text
file      ... auto x = a|\n    return 0; ...
rendering ... auto x = a    return 0; ...     ← 换行变成分隔空白，光标被映射到 `return` 的起点
```

于是 `name_position_at` 看到的是"光标落在 `return` 上",前缀为空、区间为空。

### 修法

`reading_offset_of` 增加一条**精确**规则:光标恰好等于某个 span 的 `reported` 末尾(即"刚打
完一个 token"、文件里其后没有自己的字节)时,回答该 span 的 **cooked 末尾**,而不是下一个 token 的起点。

```rust
if let Some(previous) = self.reading.iter().rev().find(|span| span.reported.end_offset() == in_the_file) {
    return Some(ReadingOffset::new(previous.cooked.end_offset()));
}
```

"精确"是刻意的:空行上的光标、注释中间的光标都不满足这条,保持原来的答案。

`name_position_at` 也加了一层兜底:名字节点找不到时(正在打、解析器恢复没把它放进 `NameExpr`),从
**token 流**里读:光标所在或光标前一个字符所在的 Identifier,token 起点到光标之间的文本是前缀,token
整体是替换区间。

### 验收

```text
`a|`   prefix "a"  replace 51..52(1 字符)  首项 anotherVec
`an|`  prefix "an" replace 51..53(2 字符)  首项 anotherVec
```

测试 `the_typed_prefix_is_the_word_being_replaced`(一个词一个 session —— 词是文件的一部分),
`cpp_code_analysis` 全套 650+ 全绿,`cpp_ls` 80 + handshake 30 全绿。

## 二十、宏的补全项、注释里的补全、以及从未被画过的 doc 命令（2026）

### ① 宏的"签名"是 `int ()` —— 身份位置不是它自己的

上一轮把宏补全项的克隆字段清空了,用户又报了一次:`FULL_NAME_FORMAT` 的详情还是 `int ()`。
根因在**另一个字段**:客户端回传 `completionItem/resolve` 的是一对 `(file, offset)`,那个 offset 取自
`DeclFact::name_range` —— 而克隆保留的是模板声明的 range,于是它指向 `int main()`。

```text
修法  收集宏时把 fact.range 一起带上,克隆里 name_range/range/scope/in_namespace 都改成宏自己的
结果  resolve 在该位置找不到声明 → 没有 detail(而不是错的 detail);注释写在 #define 上方时文档也能读到
```

### ② 注释里的两次补全

```text
/// @par|         → 命令词表(param、paragraph …),按已打的字母过滤
/// @param |       → **被注释的那个声明**的参数名
```

第二个的名单来自 fact(`parameters` 是模板头,`parameter_list` 是形参表),形参名用 inlay 的同一条
`parameter_names_from_text` 解析 —— 一处实现,两处使用。

一个坑:`///` 行在渲染里**不存在**(注释是 trivia,cook 先删掉才解析),所以这个位置只能从**文件文本**判断。
第一版用树的 `DocComment` 节点,结果 `@par` 能答、`@param ` 答不出 —— 因为后者光标正好落在注释块**末尾的换行**上,
那个 token 的祖先里什么都没有。改成按行读文本(`///`、`//!`、`/**`、`/*!`),同一个规则也顺便分清了
`// note` 不是文档注释。

### ③ doc 命令的语义高亮:从来没有画过

`push_doc_commands` 遍历 `view.root.descendants()` 找 `DocCommand` 节点 —— 而渲染里一个注释都没有,
所以这个循环在**任何文件**上都找不到东西。改成从文件文本按行扫描(`@`/`\` + 已知命令名),
offset 直接就是客户端的坐标系,不需要映射。

```text
测试  `@param`/`@return` 得到 DocCommand;`// a plain comment with @param` 与 `someone@example.com` 不得到
```

### ④ 宏展开后的补全(图 3)与"打了 a 却不过滤"(图 1)

```text
#define STD std::  →  `STD s|`  scope "std"  prefix "s"  →  cin / cout / size(实测)
`auto x = a|` / 语句位置 `a|`               →  prefix "a"/"an",replace 覆盖已打的字(上一节的坐标修复)
```

## 二十一、关键词与 Doxygen 命令的文案:一份 yaml,编译进二进制（2026）

两件事都缺,而且缺的是**文字**而不是逻辑:

```text
constexpr / noexcept / friend 之类的关键词   没有 hover
@param / @return 之类的命令                   没有 hover;命令本身也没有描述
```

### 做法:文案进 `locales/en.yml`,`rust-i18n` 在编译期嵌进去

项目已经有 `rust-i18n`(解析器的报错就是 `t!("expected primary expression")` 这套 message-as-key),
所以没有再引入 yaml 解析器:`rust_i18n::i18n!("./locales", fallback = "en")` 会把 `locales/en.yml`
**编译进二进制**,一张表就是一份译文,加语言是加文件而不是加代码路径。

```yaml
keyword:
  constexpr: "**`constexpr`** — the value or function may be evaluated at compile time. …"
command:
  param: "**`@param`** — document one parameter: `@param name description`. …"
```

```rust
// cpp_ls/src/handlers/hover/vocabulary.rs
pub fn keyword_hover(token) -> Option<String>   // key 取自 token 的**拼写**(bool 是 BoolLiteral 变体,不能按变体取名)
pub fn command_hover(name)  -> Option<String>
fn known(key) -> Option<String>                  // rust-i18n 缺 key 时返回 key 本身 —— 这就是"没有"
```

内容:51 个关键词 + 48 个 Doxygen 命令,每条一句"是什么"加一句"要知道的事"。表里没有的命令就**不弹**(八十几条
冷门命令配一句泛泛的话只是噪声)。

### 顺序:词汇表是**最后**一个问题,不是第一个

第一版把它放在最前面,结果 `this` 在成员函数里弹出的是关键词释义 —— 而分析层知道它是"指向本对象的指针",
那是更好的答案。改成:先问程序(`declarations`/`types`/`expressions`/`headers`/`macros`),没有答案再问词汇表。

还有一条:**只有光标真的在那个词上才答**。渲染把空行和注释删掉了,空行上的光标会映射到下一个 token ——
实测在 `void f()` 上一行空白处 hover 得到了 `void` 的解释。用 token 的区间映射回文件坐标再比较,越界就不答。

### signatureHelp:文件说明在上,`@param` 各归各位

```text
之前  documentation = 整段注释(含 @param 行)      parameters[i].documentation = None
现在  documentation = 去掉 @param/@tparam 行的正文  parameters[i].documentation = 该参数的 @param 正文
```

`@param value the value` 是**关于一个实参**的句子,它该出现在那个实参下面;放在签名上方是同一句话说了两遍,
而且第二遍还说错了地方。过滤按 `DocCommandKind::Parameter` 而不是按拼写,所以 `@param[in] x`、`\param x` 一起去掉;
`@note`/`@return` 这类"正文型"命令保留。

参数名匹配用的是语法树已经切好的 argument(`@param x  desc` 两个空格、描述续行都成立),不是再切一次文本。

### 验收

```text
测试  a_keyword_and_a_doc_command_explain_themselves（关键词/命令都有文案；`// @param` 不算）
      a_doc_comment_is_split_between_the_signature_and_its_parameters
handshake  a_signature_is_shown_while_the_arguments_are_typed（正文无 @param，第一个参数有 "the count"，第二个是 null）
           a_client_...hover（空行仍然答 null —— 词汇表加了区间检查）
套件  cpp_ls lib 83 / handshake 30(+5 ignored) / cpp_code_analysis lib 650
```
