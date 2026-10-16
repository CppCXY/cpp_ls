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
