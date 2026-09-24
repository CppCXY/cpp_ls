# 介入标准库：怎么进去、先做什么、代价在哪

这份文档只管一件事：**让 `#include <vector>` 之后的世界可用**。索引与语义层的决策与不变量仍在
[`index-design.md`](index-design.md)，parser 的读法与缺漏仍在 [`grammar-gaps.md`](grammar-gaps.md)——
这份是那条工作线的施工图，以及它撞上的几条旧决定。

**当前的待办队列在 [`roadmap.md`](roadmap.md)**（含 parser 侧的第一条错清单与最小复现）；这份文档保留"为什么这么做"与每次普查的数字。

## 目标，与明确不做的事

```text
要：  #include <vector> 解析得出来（于是文件能被缓存、include 图是完整的）
要：  std::vector<int> v; v.push_back(1);  里的 push_back 能答出来
要：  v. 处的补全能列出 vector 的成员
不做：打包一份 std 声明 / 一份手写模型
不做：预索引整个标准库
不做：模板实例化（std::vector<int> 的成员按主模板答，这是既有边界，不是这一轮的目标）
```

**第一条"要"与语义无关，而它的收益最大。** `index::store` 的规则是"include 解析不出来的摘要不落盘"，
而真实项目的每个 `.cpp` 都 `#include` 标准头——也就是说**今天几乎没有哪个真实文件进过缓存**。搜索路径
一旦发现得了，缓存、增量、可见性走查三件事同时开始对真实项目生效。这一条应当先做、独立做、单独量。
（发现那一步已经落地，见下面的 P0；**把它接上驱动层的那一步还没有**，所以收益目前是可兑现而非已兑现。）

## 实测（数字都要能重跑）

`docs` 里的数字如果不能重跑就不是数字。两个例子程序就是为此留的：

```bash
# 闭包：让编译器自己说要哪些文件
g++ -M -std=c++20 t.cpp | tr '\\' '/' | tr ' ' '\n' | sort -u > files.txt

cargo run --release -p cpp_code_analysis --example std_probe -- files.txt   # 闭包长什么样、哪里断
cargo run --release -p cpp_code_analysis --example std_index -- files.txt cache  # 索引它要多少钱
cargo run --release -p cpp_code_analysis --example std_query                # 端到端：按光标问成员
cargo run --release -p cpp_code_analysis --example open_project             # 驱动层：开项目 → 惰性索引 → 问
```

本机（Windows / MinGW-w64 g++ 15.1.0 / libstdc++）实测：

| 闭包 | 文件 | 行数 | 大小 | 索引（parse+scopes+facts） | 落盘 | 冷读 | 盘上 |
|---|---|---|---|---|---|---|---|
| `<vector> <string> <memory> <algorithm> <map> <iostream>` | 185 | 111,420 | 3.5 MB | **≈1.0 s** | +0.31 s | **11 ms** | **326 KB** |
| `<bits/stdc++.h>`（最坏情况） | 359 | 250,721 | 7.6 MB | ≈2.3 s（parse 1.88 s） | — | — | — |

事实总量很小：182 个文件的摘要是 **1,523 条声明 + 2,824 条宏**。

**这三个数决定了后面的策略**：冷索引一个闭包约 1 秒、热读 11 毫秒（快 90 倍）、盘上三百多 KB。也就是
"按需索引闭包 + 按文件缓存"是**可行的**，标准库不需要任何特殊通道——它需要的只是让 include 能解析。

## 闭包今天长什么样：26% 干净（两条规则之后是 42%）

```text
185 个文件：干净 48，报错 134        P1 的两条形状规则之前
185 个文件：干净 78，报错 104        P1 的第一批规则之后（见下面的 P1）
报错信息（按出现次数，注意这**不是**缺陷数）：unexpected token 2067、expected `;` after expression 1098、
expected `;` 637、expected primary expression 628、expected `}` 355 …
```

标准头会**级联**：`bits/stl_algobase.h` 一个构造没读出来，往后 190 条错。所以正确的读法是
`std_probe` 的第二段——**每个报错文件的第一个错**，那是唯一没有东西能解释它的那一条。逐条看下来是三个家族：

### 家族一：不知道那是宏（134 个里 78 个，58%）

`bits/stl_algobase.h:83` 是典型，一处坏掉、后面 190 条跟着错：

```cpp
namespace std _GLIBCXX_VISIBILITY(default)   // 83 行：名字与 { 之间夹了一个宏
{
  template<typename _Tp, typename _Up>       // 91 行：报错落在这里，但它不是原因
```

同一族在**四个位置**都出现，这也决定了修法不能只堵一个：

```text
类型之前      _GLIBCXX17_INLINE const _Lock_policy __default_lock_policy =
声明符之后    terminate_handler set_terminate(terminate_handler) _GLIBCXX_USE_NOEXCEPT;
参数表之后    _S_maximum(_Base_ptr __x) _GLIBCXX_NOEXCEPT                       （225 条错）
名字与 { 之间  namespace _GLIBCXX_BEGIN_NAMESPACE_CONTAINER / __MINGW_EXTENSION typedef … / __attribute__((…))
```

**关键的一条量化**：这 78 个文件里，第一个错所在行（含上面两行）出现的宏名，**只有 5 个（4%）是这个文件
自己 `#define` 的**，78 个（58%）来自闭包里**别的**文件。`_GLIBCXX_BEGIN_NAMESPACE_CONTAINER` 定义在
`x86_64-w64-mingw32/bits/c++config.h:488`，`_GLIBCXX_VISIBILITY(V)` 在同文件 82 行。

> **推论：文件自己的 `MacroNames` 表（parser 里已经有）对标准库几乎没用，起作用的是外部符号表。**
> 那根线已经接好了但**故意没通**：`SymbolTable` / `SymbolKind::Macro{function_like, body}` 是 parser 的
> 接口，`sema/parser_symbols.rs` 实现了它，而且明确不回答宏。接上它是这一族的主要修法。

### 家族二：`__attribute__((…))` 与 `__declspec(…)`（GNU/MS 属性语法）

`c++config.h:348`（这个文件自己的第一个错）：

```cpp
extern "C++" __attribute__ ((__noreturn__, __always_inline__))
typedef int __int128 __attribute__ ((__mode__ (TI)));
void __cdecl __MINGW_ATTRIB_NORETURN abort(void);
```

**它与家族一不是一回事，展开之后也不会消失**：`_GLIBCXX_VISIBILITY(default)` 展开的结果**就是**
`__attribute__ ((__visibility__ ("default")))`。所以"让闭包干净"= (a) 知道名字是宏 + (b) 把属性语法
当成一个可跳过的语法元素。两件事都要做，做一件只走一半。

### 家族三：真正的语法缺漏（少，可枚举）

```text
__t.~_Tp()                              伪析构函数调用（`x.~T()`）
__it.operator->()                       显式写出的运算符调用
{ operator<=>(static_cast<_Tp&&>(__t), …) }   requires 表达式里的运算符函数调用
{ ::new ((void*)__ptr) _Tp(…) }         全局 `::new`
inline constexpr bool __is_tuple_v<tuple<_Ts...>> = true;   变量模板的偏特化
```

这一族是 `grammar-gaps.md` 的既有工作方式（例子 / 现象 / 成因 / 性质 → 修 → 钉住），只是**输入从手写
探针换成了真实文件**——第 11 条维护约定说的正是这件事，而标准库是这个手段能拿到的最好的语料。

## 三个家族压出的一条旧决定到期了：宏环境

家族一里最刺眼的一个事实，`c++config.h` 486–495 行：

```cpp
#if defined(_GLIBCXX_DEBUG)
# define _GLIBCXX_STD_C __cxx1998
# define _GLIBCXX_BEGIN_NAMESPACE_CONTAINER \
	 namespace _GLIBCXX_STD_C {          // 展开成"开一个 namespace"
# define _GLIBCXX_END_NAMESPACE_CONTAINER }
#else
# define _GLIBCXX_STD_C std
# define _GLIBCXX_BEGIN_NAMESPACE_CONTAINER  // 展开成"什么都没有"
# define _GLIBCXX_END_NAMESPACE_CONTAINER
#endif
```

**同一个宏名，在两个分支里一个开出 `namespace`、一个什么都不开。** 所以"这个宏是什么"不是文件的属性，
是 **（文件 + 分支 + 位置）** 的属性——而索引层的宏查询早就是这个模型了（按翻译顺序定，只由守卫决定的
候选一律 `ConditionalCompilation`），偏偏 **parser 拿不到它**：`FileIndexer` 只喂 `ParserConfig::default()`。

这不是疏漏，是 [`index-design.md`](index-design.md) 里**写明的推迟**：

> `-D`/`-std` 不参与解析，所以摘要与宏环境无关（这就是键里没有宏环境的原因）。哪天要把 `-D` 喂进去，
> **键里必须同时把宏环境加回来**，并且抬 `FORMAT_VERSION`。

标准库就是"哪天"。`_GLIBCXX_DEBUG`、`__GLIBCXX__`、`_GLIBCXX_HOSTED`、`__cplusplus` 这一层条件决定了
半个闭包的读法，而"两个分支都按文本读"在**分支互相矛盾**时不是宽容、是错：一个分支开 namespace、另一个
不开，读成一个就是 `}` 对不上。

**所以第三层要么不做（接受标准库的一部分读不准），要么就得付这条代价：宏环境进解析器 + 键里加回宏环境
+ 抬 `FORMAT_VERSION` + 全库作废一次。** 这是这条工作线上最贵的一步，也是最不该偷偷做的一步。

## 分阶段（按"买到什么"排序，不按技术难度）

### P0 搜索路径（**已落地**：`include::toolchain`）

`#include <vector>` 现在能解析了：发现工具链的自带 include 目录，填进 `CompilerConfig::include_paths`
（全部 `is_system: true`）。一个调用：

```rust
let files = DiskFiles;
let toolchain = toolchain::discover(&files, &DiskCommands, commands.as_ref(), main_cpp, &Environment::current());
let config = toolchain.map_or_else(CompilerConfig::new, |toolchain| toolchain.config(&base_config));
```

**顺序**：`compile_commands.json` 给这个文件指定的编译器 → `CXX` → `CC` → PATH 上的
`g++`/`clang++`/`c++`/`gcc`/`clang`/`cc`。每一步都是比前一步更弱的主张：构建数据库是唯一知道"这个文件由那
个编译器编译"的东西（一个项目里两套工具链就是它存在的理由），而 PATH 是对机器的猜测，所以排最后。
带分隔符的名字当路径用，裸名按 PATH 找——和 shell 对同一个字符串的处理一致。

**买到**：include 能解析 → 文件可缓存、include 图完整、可见性查询不再因为"头没找到"而错。
本机实测 g++ 15.1.0 解出 6 条目录，`<stddef.h>` 与 `<vector>` 都能解析（`tests/toolchain.rs` 就在真机上
验这两条）。

**几个当场定下的取舍，各自都有证据：**

| 决定 | 依据 |
|---|---|
| 用 `-`（空标准输入）而不是 `NUL`/`/dev/null` | 两个编译器实测都认，且不需要知道平台；`NUL` 只在 Windows 存在 |
| **不落盘缓存**发现结果 | 一次 `-E -v` 实测 **84 ms**（5 次平均），一个会话一次。落盘要定义"这是哪个编译器"的键，而**过期的 include 路径是错答案不是慢答案**（`<vector>` 会解析到一个已经没有它的目录，后面每条声明都归错文件）。84 ms 买不起这个风险；下个会话重问 |
| 路径规范化但**保留大小写** | 这些是要**打开**的目录，不是要比对的键。折了大小写会得到 `c:/program files/…`：在大小写不敏感的卷上碰巧能用，在敏感的卷上不能用，而且用户在诊断里认不出那是自己的目录（这条是测试抓出来的：第一版折了大小写） |
| 追加而不是插到前面 | 搜索在第一个含该文件的目录停，**位置就是优先级**：`-I` 必须先于编译器自己的目录，这正是项目覆盖系统头的做法 |
| 幂等（同一目录只留第一次出现） | 第二次出现永远赢不了，而每次准备配置都长一截的配置是用更多字说同一件事（GCC 自己也会说 `ignoring duplicate directory`）。这条让"每次查询准备一次配置"是安全的 |
| `#include "..."` 那一块**不读** | 那里是只有**引号**包含才找得到的目录（`-iquote`），`CompilerConfig` 没有表达它的方式，当成系统路径会让 `#include <vector>` 找到编译器不会找的文件。实测两个编译器的引号块都是空的（这次调用没传 `-iquote`），所以实践中没丢东西；规则存在是为了将来传了参数时不悄悄放宽 |
| **MSVC 不做** | `cl` 没有 `-v`，它的目录来自 `vcvarsall.bat` 设的 `INCLUDE`——从别处启动的编辑器根本没有这个变量。本机没有 `cl`，也就没有"它到底打印什么"的证据。现在写那条路是**形状合理的猜测**，比一个说明白的缺口更糟 |

**不做**：不解析 `#include` 之外的任何编译器行为，不引入 `-D`、不引入 `-std`（那是 P3 的代价——
`-D` 一进去，宏环境就必须回到键里，见上一节）。

**那一步已经走完了**（`session.rs`）：`Session::open` 开项目时做一次发现（读 `compile_commands.json`、
跑一次编译器问搜索路径、扫一遍源文件清单），之后 `advance(n)` 一步一个文件地把闭包读进来，`view` + 五个查询
按光标回答。P0 的收益因此从"可兑现"变成**已兑现**，实测见下面"驱动层"那一节。

### P1 让闭包干净解析（**进行中**：48 → 101 个文件干净）

**按形状放宽，而不是接表**——这是这一轮最重要的一条计划修正，理由见下。

十几条规则都落在 parser 里，都**不依赖任何宏表**，因为标准库的宏名字定义在 `c++config.h`（一个**被包含**的文件）里，本文件的 `MacroNames` 没见过它，而外部表（`symbols.rs`）也没接到 includes 上。能知道的只有形状，而形状在这些地方是决定性的：

| 形状 | 规则 | 代价 |
|---|---|---|
| `namespace std _GLIBCXX_VISIBILITY(default) {`（**每个** libstdc++ 头的第一行） | 名字与 `{` 之间允许宏形状的 token（名字，或名字 + 括号组），而且**只有落在 `{` 上才算** | 合法 C++ 在名字与 `{` 之间什么都没有，所以两种读法是"`namespace std {`"与"报错"，接受宏不花任何代价 |
| `_GLIBCXX_BEGIN_NAMESPACE_VERSION` 独占一行（一处分支展开成 `namespace __8 {`，另一处**什么都不是**） | 名字后面**一串**名字/调用之后，跟的东西能**开始一个声明**（锚点或类型关键字）或**结束作用域**（`}`/`#`/EOF）时，读成 `MacroCall` | 一律**不在函数体内**生效：那里 `COUNT` 后面跟 `return` 是漏了 `;`，读成宏就会把一个人该修的错吞掉。同样**不在有表证据时生效**——顺序是"本文件的表 → 调用方的表 → 形状" |
| `__attribute__ ((…))` / `__declspec(…)` | 与 `[[…]]` 同一种东西，同一个 `AttributeList` 节点，在模板头之后、说明符序列里、参数表之后都能读 | 名字由标准保留给实现，所以按拼写匹配**有依据**：没有任何 `#define` 能把它变成别的（这与 `MY_API` 那种拼写约定是两回事，见 `grammar-gaps.md` 第 24 条） |
| `void f() _GLIBCXX_NOEXCEPT`（以及 `… ) const _GLIBCXX_NOEXCEPT`、`_GLIBCXX_NOEXCEPT_IF(noexcept(…))`、`_GLIBCXX_NOTHROW _GLIBCXX_NORETURN`、变量名之后的 `_GLIBCXX20_INIT(…)`） | declarator 的**后缀位置**上一个名字（可选带一个括号组）读成 `MacroCall`，函数和变量两个位置共用同一个助手 | 在这个位置上标识符**只有两种读法**：上下文关键字（`override`/`final`/`requires`，三者都在规则里被显式拒绝）或宏；其余合法的东西全是关键字或标点。所以一个名字不花任何合法程序 |
| `operator<(a, b)`、`x.operator<(y)`、`p->~T()` | 运算符名在**表达式**里也是一个名字：光杆的作 callee（primary 规则的名字分支少收了 `OperatorKeyword`），点号之后接运算符名或析构名 | 在表达式的位置上 `operator` / `.operator` / `.~` 没有别的读法（函数体里那个形状之所以一直"能跑"，是因为**声明**规则把它读成了转换运算符声明，所以这个洞只在 requires 表达式里露出来） |
| `::new (p) T(args)`（41 处 / 13 个文件） | 一元表达式分派里加一条"`::` 后面是 `new`"，`::` 归 `NewExpr` 所有，其余交给已有的 `new` 规则 | `::` 本来是限定名的开头，所以这条 arm 必须**在名字分支之前**问；`::` 是"要全局的 `operator new`"的意思，丢掉它两个拼写就是同一个函数了 |
| `__try { … } __catch (…) { … }`（18 个文件） | 按拼写读成 `TryStat`/`CatchStat`，与 `try`/`catch` 共用同一条规则 | 名字由标准保留给实现，且含义来自**实现自己的头文件**（`bits/exception_defines.h`：`#define __try try`）——与第 24 条同一条依据，不是约定。要求 `__try` 后面是 `{`（MSVC 的 SEH 用的是同一个拼写、另一种形状） |
| `int f() throw(int)`（63 处 / 14 个文件） | 声明符**后缀**位置上的 `throw(…)`，载荷按**类型表**读（`TypeId` + 逗号） | C++17 删掉了这个规格，但标准库还写着；这个位置不可能有 throw **表达式**（形参表已经结束），所以读法唯一。变量声明没有 `throw(…)`，于是它还能给"两条读法都成功"的 `T x(y)` 断案 |
| `if _GLIBCXX17_CONSTEXPR (cond)`、`if (cond) [[likely]]`（12 + 7 个文件） | `if` 与条件之间允许一个标识符（后随 `(`）；子语句上的属性在 `parse_statement_body` 里读，于是 then/else/每个循环体一次覆盖 | `if` 后面必然是 `(`，所以一个标识符**不花任何合法程序**；属性的位置在 C++20 里就是子语句 |
| `__cdecl` / `__restrict` / `__extension__` / `__forceinline`（1464 / 365 / 61 / 3 处） | 编译器自己的关键字，在说明符序列和声明符的限定符位置都接受；`__restrict` → `RestrictQual`，`__forceinline` → `InlineSpec`，叫用约定留作裸 token | 同样的两个条件（保留给实现、来自编译器），而这一条修的是**静默错树**：`int __cdecl g(void);` 原本读成"名字叫 `__cdecl`、后缀是宏 `g(void)`"，零诊断 |
| `extern "C++" { namespace std { … } }`（**每个** libstdc++ 头的骨架） | 链接块**不进**名字作用域，于是也不被当成 body；里面的宏规则照常生效 | 标准说链接块只影响 language linkage、不引入名字作用域，所以这一条本来就该如此（详见 `grammar-gaps.md` 第九轮第一条） |
| `template <class T> bool v<T*> = true;`（变量的偏特化） / `S<T[N]>`（数组做实参） / `void f(T[4]);`（无名数组形参） | 模板头被当**包着声明的东西**读：偏特化可以有一个 template-id 名字；数组类型实参靠"`T` 是模板参数"认出来（模板参数是文件在类型位置声明的名字，作用域是它那条声明）；回走的判据跨过尖括号表并在 `template` 处停 | 三条都是"判据的边界不在它以为的地方"，症状一律出现在**别的行**上（`expected a parameter list or an initializer` 指向一个形参、`expected a template argument` 指向 `_Tp[_Nm]`），所以三条都由同一个探针（闭包普查 + 首错定位）找出来 |

**实测**（`examples/std_probe.rs`，同一份 185 个文件的闭包）：

```text
最开始：干净 48 / 报错 134     第一个错里"行内有闭包定义的宏"占 58%
命名空间头之后：干净 78 / 报错 104
属性拼写之后：干净 81 / 报错 102
名字串（run）之后：干净 82 / 报错 100
后缀位置的宏之后：干净 87 / 报错 95     （"行内有闭包宏"降到 44%）
运算符名之后：干净 88 / 报错 94
第八轮（::new / __try / throw() / if 宏 / [[likely]] / 编译器关键字）之后：干净 97 / 报错 85
第九轮（链接块不是 body / 变量偏特化 / 无名数组形参 / 数组类型做实参）之后：干净 101 / 报错 81，消息总数 ≈2423 → ≈2252
```

**第八轮的六个构造一起算只多了 9 个干净文件，但其中一条改掉的是 1464 处静默错树**（`__cdecl`，见 [`grammar-gaps.md`](grammar-gaps.md) 的第八轮那一节和维护约定第 29 条）：那些文件本来就"解析成功"，普查数不出来。所以两个数字要一起看——`clean` 数报错，`gaps.rs` 的形状断言数错树。第九轮同理：四条里只有一条（链接块）直接把 4 个文件推到干净，另外三条把 `stl_pair.h`（72→60 条错）、`concepts`、`type_traits` 这些文件的首错往后推了一大截。

`bits/stl_algobase.h` 一个文件从 190 条错降到零头——那 190 条全部来自第 83 行的命名空间头。后缀那一条一次拿下 17 个报错文件里的 5 个，包括 `c++config.h`、`move.h`、`new_allocator.h`、`typeinfo` 这些每个文件都要用的头。

**运算符那一条只"修完"了 1 个文件（`bits/ranges_cmp.h`），但把另外 4 个推后了一整段**：`compare` 55→49 条错、`bits/max_size_type.h` 58→57、`bits/stl_construct.h` 20→16、`bits/iterator_concepts.h` 从 474 行推到 616 行。它们各自的**下一个**拦路虎都不同——这正是下面那个判断点要说的事。

## 语义侧的第一批数字（P0 之后能问什么）

P0 让 `#include <string>` **解析**了，但"解析"不等于"能问"。这一轮补上别名（`roadmap.md` §3.1）
与"文件自身守卫不算条件"之后，才第一次有跨文件答案。探针是 **`examples/std_query.rs`**：
它写一个成员访问的小文件、发现工具链、索引闭包，然后按游标问每一个访问。

本机（`<vector>/<string>/<map>/<algorithm>` 的闭包，318 个文件，g++ 15.1.0）：

```text
索引：318 个文件 3.0 s（318 解析、0 命中、2 不落盘、3 个解析不到的 include）
别名事实：4821 条声明里 305 条别名（2770 条类型事实的 11%）        examples/std_index.rs

std::string          -> 找到：stringfwd.h，kind Type，type_of Some("basic_string<char>")，scope Some("std")
std::basic_string    -> Ambiguous（前向声明 + 定义都在可见范围里）
std::vector          -> NotDeclaredHere（声明还在 _GLIBCXX_DEBUG 那一类条件里）
s.size / s.substr / v.push_back …  -> 全部失败，0/7
   但报错里的名字已经是 `std::basic_string::size`（不是 `std::string::size`）
```

**最后那一行是这一轮的证据**：失败的*拼写*已经是别名跟过去之后的类，说明"跟着 typedef 走一步"在真实闭包上生效了；
挡住答案的是**重载**——一个真实类的成员几乎总是三条五条同名声明，而现在的查找规则把"多条可见"直接判成
`Ambiguous`。那条规则在普通名字查询里是对的（`a::Widget` 与 `b::Widget` 必须分得开），在**成员**查询里是错的。

**这一轮真正买到的是中间那两行**：`std::string` 从"`ConditionalCompilation`（等于什么都不知道）"
变成"一条带目标拼写的别名事实"。挡住它的不是别名，是**头文件自己的 include guard**——每个头文件把身体包在
`#ifndef _GLIBCXX_STRING` 里，于是每个 `#include` 都"落在 `#if` 里"，可见性走查把每一步都记成有条件。
`index::deguard_the_files_own_guard` 修的就是这条（理由见 `index-design.md` 三道判据第 3 条）。

剩下两格写在 `roadmap.md` §3.1 末尾，**它们是下一步真正的活**：真实类的成员几乎总是重载
（`basic_string::size` 有三条），而现在的查找规则把"多条"直接判成 `Ambiguous`；这在普通名字查询里是对的，
在**成员**查询里是错的。

### 重载那一格修了，于是链条断在 **parser** 上（这一轮最重要的一条）

成员查找不再走 `definition()`（它的"多条可见即歧义"对*名字*是对的），改问 `declarations_in(class)`
并按声明顺序取第一条：**跳转取第一条、列表取全部**（`members_of` 就是取全部的那个查询）。
理由与测量写在 `direct_member` 的注释里。修完之后 `std_query` 依然是 0/7，但诊断输出把链条指清楚了：

```text
std::basic_string 的成员 -> 7 条，全是 typedef（value_type / allocator_type / size_type …）
其中名叫 `size` 的        -> 0 条
```

**成员函数一条都没有被索引**，因为 `bits/basic_string.h` 读不干净——而它的**第一条错**正是 `roadmap.md`
队列里的 **2.1 与 2.6**。这一条**已经修了**（见下面"修完之后"），那时写下的判断是：

```cpp
485: #if __cplusplus >= 201103L
486:   using _IterBase = decltype(std::__niter_base(__k1));
487:   if constexpr (__or_<...>::value)
489:     _S_copy(...);
490: #if __cpp_lib_concepts           // ← 第一条错：指令落在 if/else 的接缝上（队列 2.1）
491:   else if constexpr (requires {  // ← 以及 if constexpr 里的 requires 表达式（队列 2.6）
```

### 修完之后：`basic_string` 的成员从 7 条变成 117 条

**第一处**（`if`/`else` 之间的指令，`parse_if_statement`）：`basic_string.h` 的第一处错
**490 行 → 3838 行**——整个类体现在读通了；普查 **干净 74 → 76**。

**第二处**（类体里两个成员之间的指令，`parse_class_body_members`）：改前 `#if` 那一行被读成一条**成员声明**
（名字是条件里的第一个标识符），后面的成员全成了它的子孙——**无损、零诊断、形状错**。
改后 `declarations_in("std::basic_string")`：**7 → 117 条**。

**第三处已经定位到行**（`basic_string.h` 1161–1163）：两个宏站在类型前面
（`_GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR` / `size_type` / `size() const`），于是 walker 把第一个宏当成
**类型**、把 `size_type` 当成**名字**，`size` 从来没有被绑定——这就是 `std_query` 依然 0/7 的原因，
也是 `roadmap.md` §2.3 那一族（"宏在声明的最前面"）。修它的判据要在"文件自己的 `#define` 表"与
"下划线大写的实现保留名"之间选，选后者时按维护约定第 16 条写清为什么。


所以**语义线的下一块不在语义这边**：`basic_string` 是标准库里最常用的类，它的成员函数读不出来，
上面那些别名、守卫、重载的修复都没有东西可查。先做 parser 的 2.1（四个接缝是一个模式）与 2.6，
再用同一个探针量一次——`std_query` 的分母是 7，现在的 0/7 是一个**可以对比的基线**。


### 第十轮 + 第十一轮：`basic_string` 的公开接口全部读到，`std_query` **0/7 → 7/7**

第九轮结束时 `declarations_in("std::basic_string")` 有 **117 条**成员——typedef 与私有辅助函数（`_M_*`、`_S_*`）
都在，**公开接口一条都没有**。第十轮做的是 §2.1 那条接缝在 `bits/basic_string.h` 里的**九处**，以及"读坏了之后
怎么办"三条；第十一轮做的是"一个构造写在两个分支里"与"一个 token 定读法"六条，外加**同一条恢复判据在语句层
的第二次**（最后那一条把 `std::map` 的成员表救回来）。形状与读法见 [`grammar-gaps.md`](grammar-gaps.md)
的"第十轮""第十一轮"两节。

```text
declarations_in("std::basic_string")   117 → 442 条
  其中函数                              103 → 391
  成员里有                             size / length / max_size / resize / capacity / reserve / clear / empty /
                                       at / front / back / append / push_back / assign / insert / erase / pop_back /
                                       replace / copy / swap / c_str / data / get_allocator / find / rfind /
                                       find_first_of / find_last_of / find_first_not_of / find_last_not_of /
                                       substr / compare / starts_with / ends_with / contains / begin / end /
                                       rbegin / rend / cbegin / cend / crbegin / crend
basic_string.h 里该作用域的事实覆盖到        3759 行（类体是 93–3764，即整个类）
examples/std_query.rs                 0/7 → 9/9（九条查询全部答出来，而且都答在标准库自己的头文件上：
                                       s.size / s.substr / s.empty → bits/basic_string.h；
                                       v.push_back / v.size → bits/stl_vector.h；
                                       m.find / m.begin → bits/stl_map.h；
                                       (*p).size / arr[0].empty → bits/basic_string.h，其中 p 是
                                       std::string* 而 arr 是 std::string[4]——解引用与下标各要跨三步：
                                       别名、类型算术、跨文件成员查找，见 roadmap §3.2）
bits/stl_vector.h 的事实行数           37 → 469（`std::vector` 自己 0 → 113 条）
普查（128 个文件的闭包）               干净 76 → 80 / 报错 52 → 48；消息总数 1435 → 932
                                      每文件错误数：干净 80 | 只有一个 6 | 两到五个 13 | 超过五个 29
std_index                             声明 4821 → 12550；类型 2770 → 6223（别名 305 → 744）；带基类的类 54 → 155
```

**探针的设计在这一轮兑现了两次**：`s.empty` 一度答在 `cow_string.h`（那是老 ABI 的那份类），而 `m.find`
一度答"未声明"——一个 resolved 计数会把前者算成通过、把后者算成失败，只有"答在哪个文件、哪个作用域"能说出
这两件事各自是什么。

**同一轮里，"文件的首错"在三个文件上都往后走了，而干净文件数只 +3**——因为这一族修的是**静默错树**
（成员挂错了父亲，树无损、良构、多数时候零诊断）。判断这类收益只能靠 `gaps.rs` 的形状断言 + 成员数，
普查只看得见报错：第 29 条约定又一次被验证。

**同一轮里还有一条被量下来的否定结论**（假设写对了、也修好了，仍然要撤）：`bits/stl_vector.h:464` 的
`__glibcxx_class_requires(…)`（宏定义在**被包含的** `c++config.h` 里，所以宏表拿不到它）看起来只能用**形状**读，
而"名字 + 括号组 + 没有 `;`"这条形状规则在隔离里完全正确、失败后回滚的写法也对（维护约定第 32 条），
量下来却是 **398 → 287 条成员、`stl_vector.h` 的首错一行没动**。原因在 `ErrorNode` 恢复：它只前进一个 token，
所以读不下来的声明只赔上自己，后面的成员还是成员；一条宏读法会把名字和括号组一起吃掉。
**数字先于直觉**——这条规则要是"看起来对"就留下，闭包里就少了 111 个成员，而且没有任何一个数字会说出来。

**后半段（同一轮）：接缝修好只有 3/7，剩下的三个成因都在"读坏了之后怎么办"**——

* **失败的声明必须带上 `NodeEnd` 关掉**：`close_marks_above` 是"摘掉而不发结束事件"，而没配对的 `NodeStart`
  会被建树器在末尾补上，于是那条放弃掉的声明**吞掉写在它后面的一切**。`bits/stl_vector.h:192` 的一条成员
  就让 `std::vector` 一个成员都没有（`v.size`/`v.push_back` 都答"未声明"）。改成 `end_marks_to` 之后，
  `stl_vector.h` 的事实 **37 → 469**、`std::vector` 自己 **0 → 113 条**，`v.push_back` 答得出来。
* **一个声明符只有一个初始化式**：同一条成员读成"变量 `_M_grew` 被初始化两次"，第二个 `{ }` 让它没有 `;` 可收尾。
* **一个声明符可以有两个初始化列表**（一个分支一个）+ **指令与宏后缀交替**：`bits/cow_string.h:515` 的
  copy-on-write 构造函数，恢复把它的 `{ }` 当成类的收尾大括号，`class basic_string` 提前 3400 行结束
  （该作用域 159 → 48 → 修好后 **203** 条事实）。

三条合起来把 `std_query` 从 3/7 推到 **5/7**，而**普查的干净文件数一个没动**（还是 79）——
又是第 29 条：这一族修的从来不是报错，是"成员还算不算成员"。

**那剩下 2 条查询（`v.size`、`m.find`）在第十一轮的最后一步也通了**，成因不在"类读不出来"（两个类的成员早就在
索引里），而在**函数体**：`bits/stl_map.h:527` 的 `operator[]` 里有一个没有 `;` 的宏，失败的表达式语句把
`ExpressionStat` **摘掉而不发结束事件**，那个没配对的 `NodeStart` 吞掉了函数体剩下的部分与整个类体剩下的部分
（维护约定第 34 条在语句层的第二次收费）。`std-library.md` 里"`std::vector` 的声明落在条件编译里"那条判断
是错的，第十轮推翻了它（`roadmap.md` §3.1 已改）。


### 第十二轮：驱动层（`Session`），以及"第一个答案要多少文件"

**这一轮没有动 parser，也没有加新语义**：`session.rs` 把工具链发现、编译数据库、文件清单、缓存、顺序、
事件与查询串成一个"开着的项目"。标准库这条线上它买到的是一件以前量不出来的事：
**一个语言服务器从打开项目到答出第一条问题，中间要读多少东西。** 用同一批查询（`std_query` 的那 9 条，
对象换成一个含 `<string>/<vector>/<map>` 与一个本地头的 `main.cpp`）：

```text
冷启动（空缓存）  开项目（发现工具链 + 读编译数据库 + 扫描）      72 ms
                 `s.size` 的成员答出来                          第 32 个文件，2.58 s
                 整个闭包读完                                    454 个文件，9.4 s（全部要解析）
热启动（有缓存）  开项目                                          74 ms
                 `s.size` 的成员答出来                          还是第 32 个文件，但 26 ms（快 100 倍）
                 其余 422 个文件                                 3 个解析、451 个从盘上读回（命中率 99%）
查询              9/9（`std_query` 的同一批，走"游标 → 成员列表"那条路）
```

三件事值得记下来：

* **"第一个答案"和"整个项目就绪"差 422 个文件。** 这就是惰性索引的全部意义：一个语言服务器可以在
  32 个文件之后开始回答，而那 32 个是"跟着 `#include <string>` 走的"——顺序不是猜的，是
  `Worklist` 那条（打开的文件 → 它的 include 闭包 → 项目其余），只不过这里由会话的队列实现（它会活过一批通知）。
* **热启动把 2.58 s 变成 26 ms，而且解析次数是 3 而不是 0。** 那 3 个是**两个开着的 buffer**
  （未保存的文本当然不在盘上）和 1 个"include 没解析到、故意不落盘"的文件（规则二）。这是设计里写着的代价，
  在这里第一次有了数字。
* **未保存的 buffer 参与索引**：探针里 `widget.h` 磁盘写 `int on_disk;`、缓冲区写 `int in_buffer;`，
  报出来的成员是 `in_buffer`；`did_close` 之后又变回 `on_disk`。这条是"能在用户打字时工作"和"保存后才工作"的分界。

（同机重跑毫秒数在 ±10% 内波动——**文件数是硬的，毫秒数用来比大小**。）

### 判断点：队列已经从"一族"变成"长尾"

同一个工具现在多打一段**每文件错误数直方图**，因为第一处错误看不出"还差多远"：

```text
第八轮之前：干净 88 | 只有一个错 3 | 两到五个错 22 | 超过五个错 69
第八轮之后：干净 97 | 85 个文件仍报错
第十轮之后：干净 79 | 只有一个 1 | 两到五个 13 | 超过五个 35     （128 个文件的闭包）
第十一轮之后：干净 80 | 只有一个 6 | 两到五个 13 | 超过五个 29   （消息总数 932）
```

**只有 6 个文件是"修一个构造就干净"**，而 29 个文件有 5 个以上的错——那些是级联，第一个错背后还压着好几个不同的构造。这和前几轮完全不同：命名空间头一条规则拿下 30 个文件，后缀宏一条拿下 5 个，而现在的每一项只值 1–3 个文件、彼此毫无关系（变量模板偏特化、`if (…) [[likely]]`、宏站在关键字位置、`__typeof__`/`__int128` 这类 GNU 扩展、小写函数式宏独占一行）。

所以这里要**显式做一个选择**，而不是默认继续磨：

1. **继续磨**：按第一个错一个一个做。每个 1–3 个文件，100% 干净是可以到的，但按现在的斜率还要十几轮，而且每一轮的知识增量都在下降（学到的是"这个构造怎么读"，而不是"这套设计对不对"）；
2. **停下来，去处理"不变量 3 的粒度"**：88/185 已经过半。既然闭包短时间内到不了 0，那"只相信干净解析的文件"这条不变量对标准库就仍然不成立。处理它（比如给事实记一个解析干净度、由查询层降权）比再修十个构造更早解锁能力；
3. **两件事交替做**：一轮 parser、一轮语义，避免长时间只在一条线上。

**这一轮选了 3**：语义那一轮做完了不变量 3 的粒度（`DeclFact::clean`，**见下面那一节——顺带更正了第 2 条里"这才是真正挡着语义查询的那道门"这个判断：实测证明门是解析缺口，不是可信度**）；随后的一轮回到 P1，把队列前六项做完（`::new`、`__try`/`__catch`、`throw()`、`if` 与条件之间的宏、子语句上的 `[[likely]]`、编译器自己的关键字），闭包 88 → **97** 个文件干净，形状断言同时钉住了一条 1464 处的静默错树。

**下一步的队列**（`std_probe` 随时重排，且会打印文件名）：`#if` 落在 `if` 与它的 `else` 之间（`bits/stl_construct.h` 的 214 行）、模板参数表里的宏（`typename... _ArgTypes _GLIBCXX_NOEXCEPT_PARM`，`refwrap.h`）、`typedef __typeof__(…)` 与 `__int128`（`bits/stl_uninitialized.h`、`ranges_base.h`、`stddef.h`、`_mingw.h`）、小写函数式宏独占一行（`__glibcxx_function_requires(…)`）、`if constexpr (requires { … })`（`basic_string.h`）、类模板成员函数定义里的 `{ return 0; }`（`functional_hash.h`，第九轮新露出来的）、推导指引里的 `basic_string_view(_It, _End) -> …`（`string_view`）、`subrange_kind::sized ? … : …` 当模板实参（`bits/ranges_util.h`）。

**这一轮的规则被抓出来三次，每次都是"看起来对、其实把另一种读法吃了"**，三次都钉在 `gaps.rs` / `tests/symbols.rs` 里：

* 第一版问"名字后面还有没有 **declarator**"。`x = 1;` 是代价：`=` 后面没有 declarator，于是赋值被读成宏、`=` 左边空了。正确的问题是"后面跟的东西能不能**开始**一个新构造"——`=`、`*`、`(`、`.`、`[` 都是**延续**；
* 第二版把 `FOO(x);`（最令人头疼的解析）和 `TEST(A, B) { … }`（块就是体的定义）也吃了。两处都由别的规则拥有且各有测试，所以括号组后面是什么现在由 `kind_after_the_group` **一次走查**回答；
* 第三版（名字串）自己带进来两个**扫描器**错误，都由实测数字发现：`index_after_the_group` 已经返回"组的 `)` 之后"，而调用方又跳过了一个 token——于是 `size_t _Hash_bytes(const void*);` 被读成两个宏、`;` 落到了下一条声明上（`hash_bytes.h` 由干净变成报错）；以及扫描器**从不看游标自己那一对括号**，于是 `_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(_V2)` 答出 `LeftParen` 而被拒绝（`system_error` 由干净变成报错）。两条现在都有反例断言。

**还有一次是既有测试抓到的优先级错误**：形状规则一开始会在**有表证据**时也开火，于是 `MY_API Widget const w;` 里的宏被读成"独立的宏调用"，而不是"声明的一部分"——`tests/symbols.rs` 的测试说这样比有表时更差。现在这条规则是 `at_a_macro_member` 的**补集**：那个要求有证据，这个只在没有证据时开火，两者之间每个"宏站在这里"的形状恰好有一个主人。

**判据**：`std_probe` 的 `failing` 往 0 走。**0 是目标，不是 90%**——理由见下面"不变量 3 的粒度"（注意那一节已经把"0 是目标"的理由改写过了：0 服务的是**能力**，不是可信度，可信度已经由 `DeclFact::clean` 逐条回答）。

### 计划修正：**不要**先把闭包的宏喂给外部符号表

这一节写着"1. 把闭包的宏事实喂给外部符号表（家族一），这是 T4 那条设计的兑现"。想清楚之后，**它不该是
第一步**，而且它有一个必须先决定的问题。

把闭包的 `#define` 喂进解析器，意味着一个文件的摘要开始**取决于它包含的文件的文字**。而键是
`content_hash + context_hash + format_version`，`context_hash` 只有编译配置和**这个文件自己的目录**——
`ProjectIndex::insert_at` 的文档把这条写得很清楚："两个键相同的文件有相同的声明、相同的偏移、相同的
range，**以及相同的 resolved includes**"。头文件里加一个 `#define` 会让**包含它的文件**解析成另一个样子，
而那个文件的键一点没变。

项目里有一处同类问题，而且给出了答案：`IncludeFact.resolved` 也依赖文件系统，做法是**存下来、但每次命中
都重新核对**（`store.rs` 的 `resolution_still_holds`）。宏要做同样的事，代价是**每次命中都要重读整个闭包**
（实测 309 个文件 / 1.5 MB），而那正好是缓存要省掉的东西。

所以它要么进键（做不到：键必须能从文字和路径直接算出来，而解析 include 需要文件系统），要么接受"陈旧直到
监听层发现"——而 `index-design.md` 对监听层的定位是**及时性不是正确性**。这是一次真正的取舍，属于 P3 那
一层，**不该顺手做**。在那之前，形状能解决的部分（实测 58% 的报错文件里绝大多数）先用形状解决。

### P2 规模与策略（**已落地**：`SummaryStore::index_includes_from`）

**按需索引闭包 + 按文件缓存**，不预索引、不打包模型。入口是
`SummaryStore::index_includes_from(entry, budget)`：它顺着 `#include` 走，每一层都问缓存（闭包的结构不用
重新解析——摘要里就记着每条 include 解析到了哪个路径），并返回 `IncludeIndex { indexed, unresolved,
not_indexed, stats }`。

**实测（`examples/index_includes.rs`，本机 g++ 15.1.0）**：

| | 文件 | 冷 | 热 | 盘上 |
|---|---|---|---|---|
| 一个含 `"widget.h"` + `<vector>` + `<string>` 的 `main.cpp` | 309 | 3.6 s（309 次解析） | **262 ms（307 次命中，2 次解析）** | 1.5 MB |

- **不做 std 特殊通道**的理由是可测的：内容哈希键只管"文字 + 目录"，而同一个工具链的头文件在哪个项目里
  都是同样的文字——所以缓存天然按文件复用，不需要"标准库专档"这种设计。
- 什么时候索引：打开一个 `.cpp` 就把它的 include 交给这个调用（或后台段），而不是阻塞回答。因为
  **"这个 include 解析不出来"本身就是今天让文件不落盘的原因**，等它解析出来是让缓存生效的前提。
- **预算**（`IncludeBudget`）是文件数 + 深度两条，且**截断会报告**（`not_indexed` 带原因：预算、深度、
  读不到）。实测最坏闭包 359 个文件，默认 4096 留了十倍余量——而且这个数字不承重，因为截断从不静默。

**这一层第一次跑起来就抓到两个真 bug**（这正是"量"的价值，细节见 `grammar-gaps.md` 第 22 条）：标准库
里最常见的 `#include <bits/c++config.h>` 因为 `++` 不是折叠白名单里的一种 token 而没被折叠，而兜底重建
名字时又多拼了一个 `<`——于是目标变成 `<bits/c++config.h`，解析不到任何文件，包含它的文件**永远不落盘**。
修之前：169 个文件里 54 个落不了盘；修之后：309 个文件里 2 个。

**剩下那 2 个是一次很好的证据**，值得记下来：`stddef.h` 与 `ext/atomicity.h` 里有 `#include <sys/_types.h>`、
`<machine/ansi.h>`、`<sys/single_threaded.h>`——它们写在**没有任何编译会走的分支里**（`_ANSI_SOURCE` 之类），
而分析器读所有分支，所以它看见了一条编译器看不见的 include，解析不到，于是这两个文件不落盘。这就是
"两个分支都按文本读"从**解析问题**变成**缓存问题**的样子，也是下面 P3 与"不变量 3 的粒度"的量化论据。

**那个粒度问题已经定了**（下一节）：不是"整个闭包不可信"也不是"那 3 个文件降权"，而是**逐条声明**
——`DeclFact::clean`，闭包里 96% 的声明照常可用。这 2 个解析不到的文件仍然不落盘（那是缓存问题），
但它们不再让整个闭包的声明可疑。

### P3 宏环境（贵，做完 P1、量过 P2 之后再决定）

见上一节。至少在 P1 之后重新量一次：**如果按文本读两个分支只剩少量冲突，就不做**；如果冲突仍然成片
（`c++config.h` 那种"一个分支开 namespace、另一个不开"），就必须做，并且按 `index-design.md` 写好的
代价付：宏环境进键 + 抬 `FORMAT_VERSION`。

### P4 宏展开进索引（既有计划的第 6 项，标准库让它变成必需）

标准库的声明大量被宏包裹。等 P1 之后，`_GLIBCXX_NOEXCEPT` 这类**空的**宏可以靠"跳过"解决，但
`_GLIBCXX_BEGIN_NAMESPACE_CONTAINER` 这种**开出作用域**的不能——跳过它，`}` 就对不上；不跳过，它就是
一个未声明的名字。这类只能**展开**。这一条是既有计划里最贵的一块（循环、参数、`#`/`##`），标准库把它
从"可以晚点做"变成"迟早必须做"。

## 不变量 3 的粒度：已定（每条声明一个 `clean`），但"门"不在这里

不变量 3 原本是一句话："语义层只能相信**干净解析**的文件里的声明"。它对项目自己的文件成立，对标准库
不成立——除非闭包真的 0 error，否则 `std::vector` 的声明全都不可信。

**这一轮量完就定了**：粒度落在**每条声明**，字段是 `DeclFact::clean`，判据是"包含这条事实名字的**最内层
声明节点**里有没有落进一个诊断"。节点种类直接复用 `scopes::declaring_kinds`（符号层读声明用的同一张表），
所以两层对"什么算声明"不会漂移。在 `<vector>` 闭包（279 文件 / 8499 条声明，其中 5388 条来自有错的文件）上：

| 问的是哪一段 | 判为不可信 | 说明 |
|---|---|---|
| 这条事实自己的 range（= 声明符） | 106 | 漏掉类型部分的诊断 |
| **最内层声明节点**（采用） | **214** | 多出的 108 条：89 个 `Declaration` + 19 个 `TemplateDecl` = 类型部分与模板头 |
| 任何包含该名字的声明节点 | 2907 | 类体里一个错连坐每个成员，超过一半声明 |
| 整个文件 | 5388 | 顺带把 3111 条来自干净文件的也算进来 |

所以闭包里 **96% 的声明照常可用**，被标为不可信的只有 4%。

**要更正的是这一节原来的判断。** 这里曾写"不变量 3 的粒度才是真正挡着语义查询的那道门"。实测不是：
闭包里 `members_of("std::allocator")` 今天就能答（3 个成员），而 `members_of("std::vector")` 答的是
`Unknown(NotDeclaredHere)`——因为**索引里根本没有 `std::vector` 这条声明**（整个闭包里只有 3 条事实的名字
里带 "vector"，都不是它）。这是**解析缺口**，不是可信度缺口：`clean` 决定"已经读到的事实能不能用"，
读都没读到的事实它无从表态。原判断把"我们不敢信"和"我们没读到"混成了一件事，而两者要的修法完全不同。

**系统头豁免（原第 3 条出路）也不要了**，理由比"带着错误跑"更硬：它会把**恰好最需要标记的那些文件**标成
干净。原第 2 条"给事实加来源干净度"就是现在这个字段；原第 1 条（把闭包修到 0 error）仍然是 P1 的目标，
但它现在服务的是**能力**（把 `std::vector` 读出来），不是**诚实**（那是 `clean` 的活）。

## 待定

- **多编译器项目**：`find_compiler` 已经按**文件**取编译器（它拿 `for_file` 去查 `compile_commands.json`），
  所以"这个文件用 clang、那个文件用 gcc"在发现这一层是对的。没定的是**缓存键的粒度**：同一个项目里两套
  工具链意味着两份系统 include 路径，而 `config_hash` 是项目级的——这一条要在 P2 一起定。
- **`-std` 要不要一起发现**（`g++ -std=?` 的默认值是 `gnu++17` 之类）：进去就等于开始决定
  `__cplusplus`，也就更靠近 P3 的那条代价。P0 只发现路径，`standard` 留空。
- **闭包的边界**：`g++ -M` 含 `-isystem`，实测 185 个文件里有一部分是 mingw 的 C 头
  （`stddef.h`、`stdio.h`）。C 头要不要同样对待？（应当是：它们同样是"别人写的、被包含的、必须解析的"。）
- **`INCLUDE` 环境变量那条路**（MSVC）：现在不做，因为没证据。哪一轮手上有 `cl` 了，先把它打印什么记下来，
  再写解析——顺序和 `-v` 一样，先证据后规则。

## 与既有不变量/约定的关系（动手前读一遍）

| 约定 | 对这一轮意味着 |
|---|---|
| 1 `Unknown` 是一等答案 | 闭包没修完时，"这个成员在没索引到的头里"仍然是 `Unknown`，不猜 |
| 2 事实存拼写不存结论 | 宏事实存的是**拼写与形状**，所以"哪个分支活"不落盘，落盘的是"它在守卫里" |
| 3 干净解析才可信 | **这一轮的核心张力**，见上一节；标准库把这个粒度问题摆到台面上 |
| 4 改 schema 抬版本 | P3（宏环境进键）和可能的"干净度字段"都要抬；前者还要抬 `FORMAT_VERSION` |
| 5 文档是规范 | 闭包的错误普查是**队列**，写进 `grammar-gaps.md`；修一条钉一条 |






