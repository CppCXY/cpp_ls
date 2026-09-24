# 介入标准库：怎么进去、先做什么、代价在哪

这份文档只管一件事：**让 `#include <vector>` 之后的世界可用**。索引与语义层的决策与不变量仍在
[`index-design.md`](index-design.md)，parser 的读法与缺漏仍在 [`grammar-gaps.md`](grammar-gaps.md)——
这份是那条工作线的施工图，以及它撞上的几条旧决定。

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
```

本机（Windows / MinGW-w64 g++ 15.1.0 / libstdc++）实测：

| 闭包 | 文件 | 行数 | 大小 | 索引（parse+scopes+facts） | 落盘 | 冷读 | 盘上 |
|---|---|---|---|---|---|---|---|
| `<vector> <string> <memory> <algorithm> <map> <iostream>` | 185 | 111,420 | 3.5 MB | **≈1.0 s** | +0.31 s | **11 ms** | **326 KB** |
| `<bits/stdc++.h>`（最坏情况） | 359 | 250,721 | 7.6 MB | ≈2.3 s（parse 1.88 s） | — | — | — |

事实总量很小：182 个文件的摘要是 **1,523 条声明 + 2,824 条宏**。

**这三个数决定了后面的策略**：冷索引一个闭包约 1 秒、热读 11 毫秒（快 90 倍）、盘上三百多 KB。也就是
"按需索引闭包 + 按文件缓存"是**可行的**，标准库不需要任何特殊通道——它需要的只是让 include 能解析。

## 闭包今天长什么样：26% 干净

```text
185 个文件：干净 48，报错 134
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

**还差的一步**：**没有 driver 去调它**。今天 `discover` 是一个能力而不是一个行为：把
`SummaryStore` / `FileIndexer` / `Worklist` 串起来、并在打开项目时做一次发现的那个入口还不存在。
在那之前，P0 的收益（"真实项目的文件终于能被缓存"）**只是可兑现的，不是已兑现的**——这一条写在
`index-design.md` 的"现在答不了什么"表里。

### P1 让闭包干净解析

按 **include 图的拓扑序** 修，叶子在前——这不是偏好，是依赖：`c++config.h` 自己的第一个错是
`__attribute__`（家族二），而 78 个文件的错要等它先把宏定义对。落到具体动作：

1. **把闭包的宏事实喂给外部符号表**（家族一）。这是 `T4` 那条设计的兑现，接口在
   `crates/cpp_parser/src/symbols.rs`，实现在 `sema/parser_symbols.rs`（今天故意不回答宏）。
   引导顺序是个两遍问题：先解析定义者（`c++config.h` 自己定义自己需要的宏，`MacroNames` 够用），
   收获 `#define`，再带表解析它的包含者——`Worklist` 的两段正好是这个形状。
2. **属性语法**（家族二）：`__attribute__((…))`、`__declspec(…)`、`__cdecl`/`__stdcall` 这类调用约定，
   在声明的前后位置都要能跳过。
3. **家族三的语法缺漏**，逐条走 `grammar-gaps.md`。

**买到**：干净解析 → 不变量 3（"语义层只能相信干净解析的文件"）对标准库成立，于是它的声明可以被信。
**花**：这是这条线上主要的工作量，但每一族都已经是既有机制的直接应用。
**判据**：`std_probe` 的 `failing` 往 0 走。**0 是目标，不是 90%**——理由见下面的"不变量 3 的粒度"。

### P2 规模与策略（可以并行，数字已经够了）

**按需索引闭包 + 按文件缓存**，不预索引、不打包模型：

- 冷 ≈1 s、热 11 ms、盘上 326 KB（`bits/stdc++.h` 最坏 359 文件 / ≈2.3 s），这是可接受的一次性代价；
- **不做 std 特殊通道**的理由是可测的：内容哈希键只管"文字 + 目录"，而同一个工具链的头文件在哪个项目里
  都是同样的文字——所以缓存天然按文件复用，不需要"标准库专档"这种设计；
- 什么时候索引：打开一个 `.cpp` 就把它的 include 交给 `Worklist` 的后台段（而不是阻塞回答）。因为
  **"这个 include 解析不出来"本身就是今天让文件不落盘的原因**，等它解析出来是让缓存生效的前提，不只是
  为了让语义更准。

**待定的一个粒度问题**（这个必须定，不能默认）：一个闭包的 359 个文件里，若只有 3 个没修干净，
是"整个闭包不可信"还是"那 3 个文件的声明降权"？

### P3 宏环境（贵，做完 P1、量过 P2 之后再决定）

见上一节。至少在 P1 之后重新量一次：**如果按文本读两个分支只剩少量冲突，就不做**；如果冲突仍然成片
（`c++config.h` 那种"一个分支开 namespace、另一个不开"），就必须做，并且按 `index-design.md` 写好的
代价付：宏环境进键 + 抬 `FORMAT_VERSION`。

### P4 宏展开进索引（既有计划的第 6 项，标准库让它变成必需）

标准库的声明大量被宏包裹。等 P1 之后，`_GLIBCXX_NOEXCEPT` 这类**空的**宏可以靠"跳过"解决，但
`_GLIBCXX_BEGIN_NAMESPACE_CONTAINER` 这种**开出作用域**的不能——跳过它，`}` 就对不上；不跳过，它就是
一个未声明的名字。这类只能**展开**。这一条是既有计划里最贵的一块（循环、参数、`#`/`##`），标准库把它
从"可以晚点做"变成"迟早必须做"。

## 不变量 3 的粒度：标准库逼出来的一个新问题

不变量 3 今天是一句话："语义层只能相信**干净解析**的文件里的声明"。它对项目自己的文件成立，对标准库
不成立——除非闭包真的 0 error，否则 **`std::vector` 的声明全都不可信，而它恰恰是最需要可信的那些**。

三条出路，必须选一条而不是默认：

1. **把闭包修到 0 error**（P1 的目标）：最干净，也最费；
2. **给事实加"来源干净度"**：摘要记下解析错误数（或一个 `parsed_cleanly`），查询层据此降权。
   这是 schema 变更（`CODEC_VERSION`），而且"降权"是什么语义要先定义清楚；
3. **按文件判定 + 系统头豁免**：`IncludePath::is_system` 已经有这个信息，"系统头有错就当它干净"是
   一条明确的取舍，代价是标准库里的错会被静默带进来。

**倾向 1**，因为 2 和 3 都是"带着已知的错误继续跑"，而标准库是**所有人共用的同一份输入**：把它修干净是
一次性成本、全体受益；给它开豁免则会让每个下游查询都要解释"为什么这里可能不准"。但这是判断，不是结论，
P1 结束时应按实测的残留量重新决定。

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
