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

#### 实测的分解(20 个源文件共享同一堆头)

用 `cargo run --release -p cpp_code_analysis --example unit_scale -- <dir>` 复现,每个 unit 617607 token:

```text
每个 unit: render 1.2s、parse 3.3s、sweep 3.5s（其中 facts 2.5s、scopes 0.9s）→ 首读 ~7s
20 个 unit 合计 160-179 秒 —— 40 个源文件即 300+ 秒
```

`render-parse` / `render-sweep` 各只进入 **20 次**(每 unit 一次),所以这不是"重复调用",而是**每次调用都处理整份程序**。另外 `facts` 全局进入 **397 次**、`scopes` 397 次 —— 说明索引自身在 unit 读取之外还有大量同形状的工作。

#### 这里面最大的一块是 **parser 的超线性**,不是缓存

把 unit 的渲染程序落盘后用 `parse_scale` 直接量:

```text
       bytes    parse ms   ms/KB      tokens
      417133       79.3    0.195      139125
      834267      156.5    0.192      273755
     1668535      481.5    0.295      573297
     2002242      856.3    0.438      697549
     2335949     1325.5    0.581      827869
     2669656     1912.7    0.734      958035
     3003363     2558.2    0.872     1099087
     3170217     2989.5    0.966     1171875
     3337071     3202.6    0.983     1235213
```

**倍率恒定而每 KB 成本涨 5 倍**。同样大小的真实源文件(`arm_sve.h`,1.4 MB / 422611 token)只要 0.28 ms/KB,所以这不是"程序大",是**某种随文件规模累积的东西**。一倍处(0.224 ms/KB 已是纯代码的 1.2 倍)就开始偏,只是小到看不出来。

复现物在 `target/corpus/unit_program.cpp`(由 `CPPLS_DUMP=<path>` 从 `unit_scale` 落盘)。


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

### 5.1.1 落地状态(这一节随代码更新,§5.1 保持当初的计划原文不动)

| # | 代码 | 编译/测试 | 验收读数 |
|---|---|---|---|
| M0 | 已写 | **release 编译通过** | 定向测试全绿;大盘读数待测 |
| M1 | 已写 | **release 编译通过** | **已在真 MSVC 上跑出第一组数字** —— 见下 |

**第一次真实读数(M1 在 MSVC 14.51.36231 上,一个 2 文件的小工程)**:

```text
ours 40 tokens | theirs 43 tokens | matched 21 | differences 5
  3  BranchDisagreement      区域整个搬了 —— 这是 §3.2 第 1 项(内建宏表)的指纹
  2  Unknown                 形状不足以命名机制

#1  ours: portable      @ main.cpp:5     theirs: windows_only @ main.cpp:2
#3  ours: 2             @ main.cpp:5     theirs: 1            @ main.cpp:2
#20 ours: -             theirs: #        @ extra.h:1      ← `#pragma once`
#21 ours: -             theirs: pragma
#22 ours: -             theirs: once
```

**这五条差异里没有一条是"我们的读数错了"**,而且这正是这个工具的价值:它把两件早就登记在案的事情,从散文变成了可以复现的数字。

```text
3 条 BranchDisagreement  工程里没有 compile_commands.json,所以我们的内建宏表是空的:
                        `#if defined(_MSC_VER)` 判成 0,我们编译了 #else 那支。
                        编译器编译了 #if 那支。两边的分支不同 —— §3.2 第 1 项。
2 条 Unknown             MSVC 的 `-E` 保留 `#pragma once`,而 cook 把指令全部吃掉。
                        这是两条流的**真实**差别,不是错误;要么承认它,要么让 token 化器忽略 pragma 行。
```

第二条现在就有一个明确的处理方式:它属于 §3.2 第 6 项(「认识并忽略 pragma」),所以它要么变成"忽略",要么留在 `Unknown` 里等着被解释 —— 但**不该**被算成读数的错误。

M0 的代价读数(`repaired` / `uncured` / `unbalanced`)在定向测试里都是预期的值;要拿到空工程 `<format>` 的那两个症状还得在真工程上跑。
### 5.1.2 M2 其实有个真 bug,而且它正好是 §1 决策第一条说的那件事

M1 一跑起来就把它挖出来了。**同一份文件、同一个编译器,只有临时目录的位置不同**:

```text
scratch 目录                    预定义宏   我们的读数 vs MSVC
%TEMP%(当前进程写不进去)         0         2 条差异,#else 分支,Unknown
文件自己的目录(可写)             97        5/5 全匹配,0 差异
                                          (含 _MSC_VER、__cplusplus)
```

链路是这样的,每一环单独看都"没问题":

```text
msvc::Scratch::new()      cl 必须有文件才能印宏表,所以它要在某个目录里写一个空 .cpp
                          std::fs::create_dir_all(&path).ok()?    ← 失败 = None,一声不响
msvc::predefined_macros   → None
toolchain::ask_msvc       → builtin_macros 为空,note 说"编译器没能被问到"
session::assemble         → compilation_environment 里一个内建宏都没有
                          → `#if defined(_MSC_VER)` 判成 0
                          → **parser 拿到的是 #else 那一支,一个没人编译过的程序**
```

**这是"输入错了就是错了"的一个实例**,也正是 §1 决策第一条和 §2 缺口 1 说的东西 —— 只是根因比计划写的更靠前一层:不是"内建宏表没有",而是**去问内建宏表的那一步被一个 `ok()?` 吞掉了一个失败**。

已做的修复:

```text
Scratch::new(fallbacks)  系统临时目录先试,再试调用方给的目录(文件的所在目录 —— 那是
                         分析被打开时肯定可写的那一个);全部失败时**说出来**,
                         而不是留一个 0 让下游去猜
predefined_macros_with   把 fallbacks 从 discover_with → ask → ask_msvc 一路带下去
```

`0 个宏` 和 `问不到` 从此不再是一回事 —— §4 规则 3 要的就是这个:**每个 Unknown 都要带 reason**,而这里连 reason 都被吞了。


**M0 实际做的,和 §5.1 那行写的不一样,原因要记下来**:计划写的是「隔离文件**补闭合符**」——在隔离文件末尾补上它没关的 scope。实际做的是「**让那一对花括号不再配对**」。补闭合符是**加**一个 token,而解析器很可能把这个补上的 `}` 花在泄漏内部的某个构造上(这条结论在代码注释里,是上一轮量出来的)。反方向是可靠的:解析器把一个文件的 `{` 和另一个文件的 `}` 配成对,**这个配对本身已经错了**,所以取消它没有拿走任何本来正确的东西。

```text
RenderedUnit::neutralized(spans)   把指定 token 换成 `;` + 空格 —— 字节长度不变,
                                   所以原 span 表仍然有效,fact 仍然映射回它自己的文件
RenderedUnit (cook 里)             文件自己括号不平衡时不再把它的 token 丢出流 —— 见下
brace_crossings                    从"返回文件"改成"返回一对花括号的精确位置",
                                   并区分「配上的 `}`」和「节点恰好停在那里的 token」—— 见下
index_unit_rendering               只中立化那**一个**配对(而不是把整个文件从程序里拿掉),
                                   然后只把涉及的那几个文件单独再读一遍
session.rs                         删掉 `if indexed.crossings == 0` —— 读数无条件入库,
                                   代价记进 `repaired`,涉及的文件记进 `quarantined`
```

**`repaired` 和 `uncured` 是两个数,不能合成一个**,这一轮才想清楚:

```text
repaired   实际被中立化掉的 token 数(每轮新增的那些)
uncured    最后一轮解析里**还**在跨文件的配对数

一次解析报出一个"配对",而那两个 token 早就被中立化过了 —— 那不是修好,
是没东西可修了。把它算进 repaired 就是报了一个没发生的修复,
而"代价"这类数字只能往少了错,不能往多了错。
```

所以循环的退出条件是**没有新进展**,不只是"这次解析没报出跨界配对";报出 `uncured > 0` 时读数照样入库(§4 规则 1),它说明的是 `quarantined` 里那几个文件的 program 读数不可信、索引里那份是它们自己的。

**§2 里那两道闸门都拆了,这是这一轮补上的**。第一道在更下面一层:cook 时如果某个文件自己的括号不平衡,它的 token **一个都不进流**(`RenderedUnit::unbalanced` 只记名字)。这同样是"全有或全无",同样违反 §4 规则 1——一个文件有一个多余括号、八百个正常声明,结果是**一个声明都不贡献**。现在它的文本进流、名字照样记录,而它造成的跨文件配对由上面那层修掉。

**这个改动让 `index_unit_rendering` 从"改进"变成了"承重"**,不能不说清楚:

```text
改之前   不平衡文件的 token 被丢掉,流天然是平衡的
改之后   流带着这个不平衡,唯一让下一个文件不掉进泄漏 scope 的东西就是那层修复
```

所以这条链现在有两端测试:`tests/translation_unit.rs` 的 `a_file_that_does_not_balance_still_contributes_its_tokens` 钉 cook 这一端(流**故意**是不平衡的),`session.rs` 的 `a_file_that_does_not_balance_its_braces_is_named_and_still_read` 钉读取这一端(后面的文件仍被自己的 namespace 收着,**而且**不平衡的文件保住了自己的声明)。

**M1 实际做的**:

```text
src/align.rs                     纯函数:tokenize_preprocessed / align / Reason / Report / unit_tokens
examples/align_preprocessor.rs   跑真编译器的那一半;`--ours-only` 时不需要编译器,所以没装 toolchain 也能记录读数
```

§5.1 的 M1 验收写着「差异有分类、**可重复**、进 CI」,所以工具有一个 `--record` 模式:不打印给人看的报告,只打印**两次运行可以逐字节比较**的那几个数——一行 `summary`、每种机制一行、每个文件一行(**按路径排序**,不是按 token 数排序)。

两条设计上的选择,都是为了让它真的能当基线用:

```text
不记录 forty 条差异本身   否则"换了一批差异"会让基线失败,而单调性并没有被违反
零 token 的文件也要记一行  某个头文件突然不贡献 token 是最常见的回退,
                          只列非空文件会把它表现为"少了一行"而不是"数变了"
```

一个**只有做了才会发现**的坑,记在这里因为它决定了分类的形状:**一个 edit 比一个机制更细**。`#define SIZE 1024` 我们没展开,是**三个** edit——一次替换加一次插入——一个一个看会说成"`UnexpandedMacro` 加 `MissingExpansion`":两个工作项,而实际发生的是一个宏。所以 `Difference` 带着它所在**连续段**的形状(`run_ours` / `run_theirs`),分类读这个形状,报告的计数是**段**的计数而不是 edit 的计数。

另一个只有做了才会发现的坑,是**连续段的边界是模糊的**:一次替换后面紧跟一个多出来的 token,和"一个宏展开成两个 token"在 diff 里完全一样。这不是分类器的缺陷,是 diff 的信息量上限;`Difference::run_ours` 的作用就是把这个模糊**显示出来**而不是藏起来。

**第三个坑,而且它是个真错误而不是取舍**:两条流的**路径拼写来自不同的地方**——

```text
我们      RenderedUnit::files   ← include resolver 记下来的,已归一化,Windows 上大小写已折叠
编译器    行标记里的那个字符串   ← 编译器想怎么印就怎么印,常常是原样、`\` 分隔
```

不处理的话,`MissingHeader` 那条规则(唯一一条**确定性**规则,§5.0 说它"不是我们的意见")会在拼写不同的那一刻对**每一个文件**成立——报告会凭空造出一个不存在的头文件,而实际上一行分隔符不同而已。所以两条流进比对之前都过同一个 `normalize_path`(crate 里唯一一处回答"这两个拼写是不是同一个文件"),大小写折叠按平台的规则走,和 resolver 的判断不可能不一致。

这条记在这里是因为它**差点被当成"跑起来才知道"的东西**:它是静态可推的,只是推它的地方在两条流的来处,而不是在比对里。


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

**一处措辞要改**:第二行。`crossings` 这个字段在 M0 里改名成 **`repaired`**(`IndexedUnit` /
`UnitReading`),而且旁边多了 **`uncured`**,因为它的意思从"还没修好的跨文件括号对,非零就拒绝整份读数"变成了"**为了修好,中立化掉了多少个 token**"。它不再是闸门,所以"只能减少"这条对它的含义也变了:**它应该降到零,但降到零不是入库的前提**——非零时读数照样入库,只是那几个文件被单独读过。真正只能减少的是 `unplaced`、`uncured` 和 `unit errors`。


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
type_traits 仍被隔离      → M0 两道闸门都拆了,「隔离」这个机制本身没有了:
                            cook 不再因为括号不平衡丢掉文件的 token,
                            读取也不再因为跨文件括号对拒绝整份读数。
                            剩下的语法缺口归 M6
栅栏仍是事后的            → 被 M0 的"拆闸门"取代:跨文件括号对不再是拒绝理由,
                            而是被就地取消配对,并记进 `repaired`
```
