# 做一个真正的 C++ 前端:预处理器、解析器、语义分析

> **一句话**:clang 和 clangd 能做对,靠的不是"更聪明的容错",而是**六个具体机制**;我们把它们造出来,
> 而不是继续补语法形状。
>
> **本文件是决策与路线图,不是规范。** 落地之后结论写进相应模块的文档注释,这份文件随里程碑推进而修改。

---

## 0. 这份文档要回答的问题

现象(实测,空工程只有 `#include <format>`,149 个文件):

```text
crossings 0 | unplaced 0 | errors 22
quarantined ["format", "type_traits", "__msvc_ranges_tuple_formatter.hpp", "memory", "atomic"]

declarations_in("std")      = 71        ← 正常应有 1259 条
members_of(std::string)     = NotDeclaredHere
members_of(std::format)     = NotDeclaredHere
```

`format` **自己在被隔离的名单里**。它不是"解析得不好",是**被判出局之后拿一份只有自己 token 的流单独解析**,
而 MSVC 的头由 `_STD_BEGIN … _STD_END` 包着,单独解析一旦配错括号,`namespace std` 关不上,文件里所有成员
落进一个没关的 scope —— 等于这个文件不存在。"没有 std 提示"和"format 有报错"是**同一个原因**。

**"我们补了 61 个语法错误到 0,`std::string` 依然查不到"这件事本身就说明:决定成败的不是错误数量。**
补语法的收益被下游的"全有或全无"闸门吃掉了。

这份文档记录:为什么这不够、clang 到底做了什么、我们要造哪六样东西、先造哪个、怎么验收。

---

## 1. 决策

**方向:补 parser、补工具链发现、补内建默认宏对齐编译器本身。目标是做一个和 clang 同构的 C++ 前端。**

理由,按分量排序:

1. **`#if` 的答案是原理性的,不是能补完的语法缺口。** 现在未定义标识符求值为 `0`(见 `RenderedUnit::unbalanced`
   的文档注释),于是 `#if defined(_PREFAST_)` 是"可判定"的 —— 而它把 `/analyze` 头拉进了一个从不请求它的程序。
   **我们展开出的可能压根不是真实配置的那条分支,然后再去解析它。** 语法补得再多,输入错了就是错了。
2. **clang 源码公开,架构公开,这正是"敢做更大"的依据。** 我们要造的**不是编译器**:没有代码生成、没有后端、
   没有优化、没有 ABI。这是 clang 前端的一个真子集,而且是设计空间已经被验证过的子集。
3. **每一步都是可验收的,而且验收标准可以是外部真值。** 见 §5.0:同一份文件,`cl.exe /E` 的输出就是答案。
   这把"parse 正确"从意见变成了测量。

**明确不做的**(写在这里是为了以后不被诱惑):

| 不做 | 为什么 |
|---|---|
| 代码生成、后端、优化 | 我们不是编译器 |
| ABI(名字修饰、布局、调用约定细节) | 语言服务器不问这些 |
| 完整的语义分析与模板实例化 | clangd 也不做完:`UnparsedFunctionBody` 就是"用到才解析"。§3.3 |
| 复刻 clang 的每一处设计 | 移植**机制**,不是移植代码 |

**一条贯穿所有里程碑的硬规则**(见 §4):**任何一层都不允许因为"不确定"而放弃已经确定的答案。**
这不是可以和 parser 分开做的妥协条款 —— 它是纪律。`std::format` 消失不是因为语法错,是因为闸门把局部缺陷
放大成了全局故障;补 parser 的同时必须一起拆。

---

## 2. 现状:六个缺口

逐条对着 clang 的做法看。左列是 clang(设计事实),右列是我们的读数或代码里的原话。

### 缺口 1 —— 条件编译在猜

| clang | 我们 |
|---|---|
| 内建宏表由**驱动**按 target、语言标准、命令行生成;`__has_include` 真查文件系统,`__COUNTER__` 真计数,`#pragma once` 真记账 | 只有 `compile_commands.json` 里的一部分;未定义 → `0`;`__has_include` / `__COUNTER__` / 内建宏都没有 |

**证据**(代码注释原话):`an identifier nobody defines evaluates to 0 in #if, so #if defined(_PREFAST_) is
decidable — and answering Unknown there is what pulls the /analyze header into a program that never asks for it.`

**这是当前最贵的一条**:它污染的是**输入**,下游所有层都在处理一份不该存在的程序。

### 缺口 2 —— 没有 preamble,每次从零

| clang | 我们 |
|---|---|
| 每个文件一个 **precompiled preamble**:预处理到主文件之前的全部状态(宏表、声明、文件边界)打成 PCH;主文件改一行 → preamble 不动,直接复用;**几十次击键之间系统头一次都不重解析** | `read_the_unit` 每次**重新渲染 + 重新解析**整个单元 |

**证据**:同一个 `<format>` 工程,一次 `read_the_unit` 是 **27 秒级**(199 文件、540 万字节、106 万 token)。

### 缺口 3 —— 函数体全解析

| clang | 我们 |
|---|---|
| 函数体记成 `UnparsedFunctionBody` 边界,**用到才实例化**;`-fdelayed-template-parsing` 是同一思路的显式开关 | 全解析。`basic_string` 那 94 KB 里绝大部分是模板成员体 |

对模板密集的 STL,这是数量级的差别。

### 缺口 4 —— 一条同步流水线挡在热路径上

| clang | 我们 |
|---|---|
| 后台索引用轻量模式(跳函数体)遍历工程写磁盘索引;**前台不等它**,先答 preamble + 当前 TU,背景补齐只是让答案变多 | `index_everything` + `read_the_unit`,同步、全量、开盘就做 |

### 缺口 5 —— 一处不确定就全丢(不是语法问题,是闸门问题)

| clang | 我们 |
|---|---|
| 恢复是"这里读不懂 → 记一个错误节点 → 继续"。**没有**"有一处括号跨界就拒绝整份程序" | 两道全有或全无的闸门:<br>① `if indexed.crossings == 0` —— 一处跨文件括号对 → **整份读数一条事实都不入库**<br>② 隔离文件用 `only(file)` 单独解析,**不给任何闭合上下文** |

**证据**:`std::string` 曾经 `NotDeclaredHere`,而修掉 `mentions_a_qualified_name` 之后立刻有 204 个成员。
**真正卡住的往往不是语法,是闸门。**

### 缺口 6 —— 配置不确定时不说

| clang | 我们 |
|---|---|
| `compile_commands.json` 缺失时用内建配置(按 target 给内建宏),并在诊断里**明说是猜的** | 猜完就当确定的用 |

---

## 3. 目标架构

### 3.0 一个必须先说清的事实:预处理**替代**掉一类 workaround

现在解析器和作用域构建里有一整套"宏感知"逻辑:`walk_children` 里的 `OpenedByBody::Opens/Closes`、
`parse_a_macro_that_stands_for_a_declaration`、`RenderedUnit::unbalanced`、`brace_crossings`、`only(file)`、
栅栏本身。

**真正做预处理之后,解析器看不到 `MacroCall`** —— `_STD_BEGIN` 就是一个 `extern "C++" { namespace std {`。
那一整套机制的存在理由是:**编辑器的缓冲区路径是按文件、未展开的文本解析的**,而那条路径必须处理"这个宏会不会
开一个花括号"。这是缺口 1 的直接后果。

**所以"补预处理器"不是给现有架构加一层,它是删掉一层。** 这是这个方向最大的结构性收益,也是"越走越远的
workaround"能被回收的地方。

### 3.1 分层

```text
lexer          已有(cpp_parser::lex)
  ↓
preprocessor   缺:条件求值、内建宏、__has_include、__COUNTER__、#pragma once、pragma 栈
  ↓
cook/stream    已有骨架(RenderedUnit / RenderedCooked),要加:内建宏表、配置来源
  ↓
parser         已有(cpp_parser),要补:剩余 C++ 语法、diagnostic 质量
  ↓
sema           部分(build_scopes / build_facts),要补:名字查找、类型推导、限定的模板处理
  ↓
index/query    已有(ProjectIndex),要补:缓存、降级
```

### 3.2 预处理器要做到什么程度

**必须**(缺一个就会走错分支):

1. 内建宏表,来源优先级:`compile_commands.json` → `cl.exe` / `clang` 探测(缺口 1 的"工具链发现")
2. 未定义标识符在 `#if` 里 → `0`(**这已经是 C++ 规则,不是猜测**;要改的是让它**不该出现未定义**)
3. `defined` / `__has_include` / `__has_include_next` / `__has_cpp_attribute` / `__has_builtin`
4. `__COUNTER__` / `__LINE__` / `__FILE__` / `__DATE__` / `__TIME__`
5. `#pragma once`、`#pragma push_macro` / `pop_macro`(MSVC 头大量用)
6. `#pragma warning` / `#pragma pack` 之类的"认识但忽略",**不要**因为不认识就当语法错误

**可以晚做**:`_Pragma`、模块、`#embed`、`#import`。

### 3.3 语义分析做到什么程度

**目标函数是"回答查询",不是"编译"。** 以此划线:

| 做 | 不做 |
|---|---|
| 名字查找(限定、非限定、ADL 的近似) | 完整重载决议 |
| 类型推导(auto、decltype、模板实参从形参推) | 完整模板实例化 |
| 类的成员表、基类链、using、访问控制 | ABI 布局、虚表 |

**模板可以按需实例化**:clangd 也不做完,它只是"用到才做"。这条同时是缺口 3 的答案。

### 3.4 缓存(preamble 的等价物)

我们不需要 PCH 的字节格式,**需要的是它的不变式**:

```text
一个文件的渲染结果 = f(它的文本, 它的宏环境, 配置)
宏环境 = 时间线上它之前的一切
```

所以缓存键就是 `(content_hash, context_hash)` —— 代码里已经有 `SummaryKey` 的两半结构。要补的是:
**渲染结果也要按这个键缓存**,而不只是 summary。同时补"改一行 → 只有这个文件的渲染失效"。

---

## 4. 硬规则:局部错误 → 局部后果

**这条是纪律,不是里程碑。** 如果补 parser 的时候不拆闸门,那么下一个语法缺口还会让整个索引为空 ——
而这几天已经反复发生过。

```
规则 1  任何一层都不允许因为"不确定"而放弃已经确定的答案
        crossings != 0 不该阻止入库;要记下来,并只把泄漏附近的标 Unknown

规则 2  被隔离 / 解析失败的文件,最多只失去它自己
        单独解析时补上它没关的 scope 的闭合符(补的是隔离文件自己的,不是泄漏文件的)

规则 3  任何 `Unknown` 都必须带 reason,并且 reason 要能指导用户
        "NotDeclaredHere" / "Ambiguous" / "BecauseTheConfigurationIsAGuess" 是三种不同的东西
```

代码里本来就有三值 `Known<T>` 和 `Unknown(reason)` —— **这两条闸门违反了它自己定的哲学**:把"我不确定"
升级成了"我什么都没有"。

---

## 5. 工作流与验收

### 5.0 验收标准是外部真值,不是我们的意见

**方法:同一份文件,让真编译器预处理一遍,和我们的流对齐。**

```text
cl.exe /E /d1PP main.cpp     或     clang -E -dM main.cpp
        ↓
它展开出的 token 序列 / 宏表 / 存活的分支
        ↓
和 cook 出来的 RenderedUnit 比:token 数、文件边界、每个 #if 分支的取舍
```

这是这份文档里**最值钱的一条**:它把"我们 parse 得对不对"从争论变成数字,而且数字的来源不是我们自己。
`cl -d1PP` 能直接给出 MSVC 的预定义宏表,正好喂给 §3.2 的第 1 项。

### 5.1 里程碑

| # | 工作流 | 交付 | 验收 |
|---|---|---|---|
| **M0** | **拆闸门 + 隔离文件补闭合符** | `crossings != 0` 照常入库;`only(file)` 补闭合 | 空工程 `<format>`:`std::` 有提示,`members_of(std::format)` 有成员 |
| **M1** | **对齐验证工具** | 一个 example:对同一份文件跑真编译器预处理,和我们的流对比 | 差异有分类、可重复、进 CI |
| **M2** | **工具链发现** | 找到 `cl.exe` / `clang`,读出 target、标准、内建宏表 | 预定义宏集合与 `cl -d1PP` 对齐(逐条 diff) |
| **M3** | **预处理器补完** | `__has_include`、`__COUNTER__`、`#pragma once`、`push_macro`/`pop_macro`、认识并忽略 pragma | M1 的流差异降到可分类的少数几类 |
| **M4** | **渲染缓存** | 按 `(content_hash, context_hash)` 缓存 `RenderedUnit`;改一行只失效一个文件 | 第二次 `read_the_unit` 不再 27 秒;击键路径不触发全量渲染 |
| **M5** | **懒解析函数体** | 函数体记边界,用到才解析 | 索引耗时与内存读数;答案不回退 |
| **M6** | **补完剩余语法** | 逐个缺口,每个带一个语法测试 + M1 的读数变化 | 语法测试 + 流差异 |
| **M7** | **后台索引 / 前台不等** | 索引换成后台,查询先答已有的 | 开盘可用时间 |

**M0 先做的理由**:它不依赖任何其他里程碑,改动小,而且**直接消掉用户已经看到的那两个症状**。
M1 放在 M0 之后、M2 之前,是因为**它让后面每一步都有数字可依**,而"没有数字"正是这几天反复拉扯的原因。

### 5.2 每一步的"不许回退"清单

补 parser 的过程里,下面这些读数**只能变好或持平**,变差就是回归:

```text
流差异(M1)                  只能减少
crossings                    只能减少
unplaced                     只能减少
unit errors                  只能减少
members_of 的抽样成功率       只能上升
单元读数耗时                  除 M0 外只能下降
```

---

## 6. 诚实的规模评估

**这是多年的工程量,不是一个季度。** 摆在这里,是为了以后每次想抄近路时能看见:

| 块 | 量级 | 我们的对应 |
|---|---|---|
| clang 预处理器 | 数万行,几十年边界情况 | 骨架有,机制缺一半 |
| clang 解析器 | 十万行级 | 已有,缺口在补齐 |
| clang Sema | 十万行级,最大的一块 | 只有作用域和事实 |
| clangd(preamble、索引、降级) | 数万行 | 只有索引骨架 |
| libc++ / libstdc++ / MSVC STL 的实际检验 | 无法估量 | 这是真正的老师 |

**但我们只要"回答查询"这个子集**:没有代码生成、没有后端、没有 ABI、没有完整实例化。
按这个裁剪,它仍然很大,但**是可达的、而且每一步都能验收**。

**风险,写在明处:**

1. **半途而废的风险最大。** 这条路的收益在 M2–M4 之后才明显;**如果 M0、M1 不做,前面几个里程碑连数字都没有。**
2. **MSVC STL 是三个 STL 里最难的一档**(模块宏、`_EXPORT_STD`、`_MSVC_CONSTEXPR`、`/analyze`)。
   应当同时用 MinGW / libc++ 做交叉检验,避免把 MSVC 特有形状当成通用形状。
3. **"和 clang 一样"不等于"和 clang 逐字一样"。** 移植机制,不移植实现。

---

## 7. 与既有文档的关系

| 文档 | 关系 |
|---|---|
| `docs/plan-units.md` §45 | 这一轮之前的语法修复与测量;**§45.4 的登记项被本文档接管** |
| `docs/review-suggestions.md` 第十轮 | 同上一轮的过程记录 |
| 各模块顶部文档注释 | **真正的设计契约**;本文档是路线图,结论落地后写进那里 |
| `docs/plan-frontend.md`(本文件) | 决策与里程碑,随推进修改 |

**登记项的去向**(§45.4 与第十轮里剩下的两件):

```text
type_traits 仍被隔离      → M0(补闭合符)与 M6(语法)一起处理
栅栏仍是事后的            → 被 M0 的"拆闸门"取代;跨文件括号对不再是拒绝理由
```
