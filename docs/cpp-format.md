# C++ 格式化器：与 clang-format 同配置、同输出

**结论先行。**

```text
1. 不做 AST pretty-printer。clang-format 自己就不是：它是 token 流 → UnwrappedLine → 标注 →
   带惩罚的贪心换行器 → WhitespaceManager → 输出。我们要"效果一样"，就必须用同一个模型；
   AST 只用来消除歧义（这个 `{}` 是块还是初始化列表），永远不做"唯一真相"。
2. 参照物钉死在 clang-format **22.1.3**（本机 VS 2026 / 18 自带：`C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\Llvm\x64\bin\clang-format.exe`）。文档早先写的是 15.0.1（VS 2022 自带），那台构建已经不在这台机器上；数字按 22.1.3 重测。clang-format 的
   输出随版本变化，不钉版本就没有"一样"这个目标。
3. 精度靠差分测量，不靠读风格文档：oracle 就是 clang-format 本身，语料是真实工程
   （EmmyLuaCodeStyle 自身 198 个 C++ 文件；整个仓库 1 138 个 / 12.9 MB / 339 081 行）+ 模板重的 libstdc++ 头。
4. 三条硬不变量，写进类型里而不是靠自觉：
     只改空白（输出与输入的 token 序列逐 token 相同，注释逐字保留，字符串/字符字面量内部不动）
     幂等（format(format(x)) == format(x)）
     稳定（format(clang_format(x)) == clang_format(x)，对 oracle 的输出再格式化必须零变化）
5. 分六段做：P0 配置 + token/行模型 + "只排空白"（ColumnLimit 0 语料），P1 缩进/括号/对齐，
   P2 换行器（最长），P3 范围/禁用/includes，P4 宏与预处理，P5 性能与编辑器集成。
   P0–P1 就能覆盖工程最常用的配置（ColumnLimit 0 的 CLion 风格），P2 才需要惩罚表。
```

---

## 一、为什么不是 AST 打印

一个"读 AST、按节点打印"的格式化器在这三件事上必然和 clang-format 分道扬镳：

```text
① 宏。`DECLARE_HANDLE(HWND)`、`_STD_BEGIN`、`Q_UNUSED(x)` —— 展开后才有语法，而格式化器
   不能展开（输出必须保留宏调用本身）。clang-format 的答案是把不认识的构造当作
   "宏块"（MacroBlock）保守处理；AST 打印没有这个概念，它要么失败要么按某种猜测重排。
② `#if`。同一个文件的两个分支可以同时是半句 C++（`#if` 里外各半个表达式很常见）。
   格式化器不预处理器，#if 的每个分支各自成一段格式化，互不"补全"。
③ 坏代码。编辑到一半的缓冲区（少一个 `}`、多一个 `;`）必须能格式化，这是编辑器的日常。
   我们的 CST 是 **lossless** 的（不变量 I1：任何输入都得到一棵完整、良构、不丢 token 的树），
   所以 token 层面的重排永远可行；AST 打印则要求树在语法上成立。
```

反过来，我们的 CST 又比 clang-format 手里的东西多：它自己是"轻量 `UnwrappedLineParser` + 标注器"，
而我们有完整语法树与 lossless 的 trivia（空白与注释都在树里）。**用法要克制**：结构只在
"括号配对、块 vs 初始化列表、`for(...)` 头、模板尖括号"这几个问题上被参考，换行与缩进的
决策仍然走 token 模型。这条边界是这份设计的核心，也是后面每个模块的约束。

## 二、目标与非目标

**目标（可验收）**

```text
G1 配置兼容：clang-format 15.0.1 的配置键全部能解析——`-dump-config` 打印 **136 个顶层键 + 58 个嵌套子键**（`BraceWrapping`、`AlignConsecutive*` 那几组的展开），共 194 行；
   7 个内置 preset（LLVM/Google/Chromium/Mozilla/WebKit/Microsoft/GNU）+ BasedOnStyle + 嵌套表
   （BraceWrapping、IncludeCategories 之类）+ `---` 多文档 + `#` 注释；
   加载器还要实现 `-style=file` 的向上查找与 `InheritParentConfig`（后者是指令，不是样式键）。
G2 输出相同：语料上 byte-identical。分级指标见 §六（P0 阶段先做 ColumnLimit 0，再上换行）。
G3 只改空白：token 序列、注释文本、字符串字面量内部逐字节不变（§七的不变量，结构性保证）。
G4 可用作 drop-in：CLI 覆盖编辑器插件常用的子集（`-i`、`-style`、`-fallback-style`、`-lines`、
   `-output-replacements-xml`、`-dump-config`、stdin→stdout），并可接进本仓库的 LSP。
G5 性能：≥5 MB/s（clang-format 15 在本机普通工程代码上约 1.4 MB/s、模板重头 0.45 MB/s，见 §八）。
```

**非目标（第一阶段明确不做）**

```text
N1 不发明风格。所有"更好看"的想法都不进代码：输出只由配置决定，而配置的行为以 clang-format 为准。
N2 不展开宏、不改 `#define` 的文本（沿行只做对齐/缩进）；不做 `-f`/宏展开语义。
N3 不动引号、注释内容、字符串拼接、数字字面量的大小写。ReflowComments 打开时才碰注释的**换行**。
N4 不做 C++ 以外的语言（Proto/Java/ObjC 的配置键解析但不生效；`Language:` 认 Cpp 子集）。
N5 不追求与 clang-format 不同版本一致。版本漂移是已知风险，见 §十一。
```

## 三、clang-format 的模型（我们要对齐的那台机器）

按 15.0.1 的公开实现，一句话一个阶段：

```text
源文本 ──lexer──▶ token 流（含空白/注释 trivia）
      ──UnwrappedLineParser──▶ UnwrappedLine 序列（逻辑行：语句、块、# 行、宏块）
      ──TokenAnnotator──▶ 每个 token 的类型（指针/引用/二元/一元/模板尖括号/lambda 引导符/…）
      ──ContinuationIndenter──▶ 逐 token 走一遍状态栈，决定"在哪儿断、下一行缩进多少"（惩罚式贪心）
      ──WhitespaceManager──▶ 把决定的空白写回去；再跑一遍**对齐**（连续赋值/声明/宏/尾注释）
      ──▶ 输出（只替换空白区间）
```

三个要点，决定了实现的形状：

```text
① 它不是"最优换行"，是**带惩罚的贪心**：每个断点先算一个代价（PenaltyBreakBeforeFirstCallParameter,
   PenaltyBreakString, PenaltyBreakComment, PenaltyBreakAssignment, PenaltyReturnTypeOnItsOwnLine,
   PenaltyExcessCharacter …），在容得下的前提下尽量少付。所谓"一样"就是"同一张惩罚表和同一个
   状态机"，不是"看起来差不多"。
② 括号内是一个状态栈（ParenState：缩进列、是否已经断过、是否 bin-pack、开括号的列……），
   每个嵌套层一套；`MustBreak` 与 `CanBreak` 的传播是整台机器的骨架。
③ 对齐（AlignConsecutiveAssignments / Declarations / Macros、AlignTrailingComments）是**后处理**：
   先把行断好，再在相邻行之间补列。它和换行器共用一套"token 在最终行的哪一列"的信息。
```

## 四、我们的架构

新 crate `crates/cpp_formatter`（lib + `cppfmt` bin；仓库里已经有这个空骨架，本设计沿用它的名字），只依赖 `cpp_parser`（取 token 与结构）与
`clap`（CLI）。不改 `cpp_code_analysis`：格式化是**文本层**的工作，绝不走 cooked 读数，
也不碰 `ReadingOffset` 那套渲染坐标。

```text
crates/cpp_formatter/src/
  config.rs      .clang-format 的 YAML 子集解析、preset 表、BasedOnStyle 解析、键校验、错误信息
  presets.rs     7 个 preset 的**生成**表（见 §五），与 -dump-config 的 golden 对比测试
  tokens.rs      从 CppSyntaxTree 取 token + 取 trivia 区间；display column 计算（tab/UTF-8）
  line.rs        UnwrappedLine 构造：语句边界、括号嵌套、块/初始化列表、# 行、宏块
  annotate.rs    token role 标注（30 余种类别，见下）
  break.rs       换行器：状态栈 + 惩罚表 + 贪心（与 clang-format 的 ContinuationIndenter 同构）
  align.rs       对齐后处理（连续赋值/声明/宏/尾注释、数组列对齐）
  render.rs      只替换空白区间地输出；同时产出 replacements（供 LSP/`-output-replacements-xml`）
  range.rs       -lines / clang-format off·on / DisableFormat 的行范围集合
  includes.rs    SortIncludes / IncludeBlocks / IncludeCategories / IncludeIsMainRegex
  lib.rs         管线：config → tokens → lines → annotate → break → align → render
  bin/cppfmt.rs  CLI
```

### 4.1 输出方式：替换空白，而不是重新拼 token

```rust
struct FormatToken {
    kind: CppTokenKind,          // 来自 cpp_parser，词法层面唯一真相
    text_range: Range<u32>,      // 在源文本里的字节区间
    space_range: Range<u32>,     // **它前面那段空白/注释 trivia 的区间**（唯一会被改写的区间）
    // …标注结果、断点状态、列号
}
```

渲染时输入是 `&str`，输出是 `String`：按 token 顺序走一遍，每到一处先把 `space_range`（两个 token
之间那段 trivia）替换成**决定的空白**，再把 token 自身的 `text_range` 原文搬运。于是
"只改空白"不是测试出来的性质，而是渲染器唯一的代码路径——token 保全由构造保证。
（骨架现在用的是 `src/main.rs`；P0 把它改成 `src/bin/cppfmt.rs` 或给 `[[bin]]` 起名 `cppfmt`。）

### 4.2 行模型：`UnwrappedLine`

```text
UnwrappedLine { tokens: Vec<FormatToken>, level: usize, kind: { Code, Directive, Macro, Comment } }
```

构造规则（从 CST 与 token 流一起读，但以 token 流为主）：

```text
· `;` 在括号深度 0 结束一行（`for(;;)` 里的 `;` 括号深度 1，不结束）
· `{` / `}` 在深度 0 各自成行边界；块 vs 初始化列表由 CST 判定（DeclFact/BraceInit 节点）
· `#` 行（指令）自成一类行，其内部的续行（反斜杠换行）属于同一行
· 顶层宏调用（首 token 是全大写的标识符、后面跟成对括号、语句尾是 `;`）成 MacroBlock，
  内部只做保守空白（这是 clang-format 的 MacroBlock 行为）
· 注释行：单独成行；行尾注释属于前一行（对齐阶段处理）
```

### 4.3 标注：token 的"角色"

标注是换行器唯一的输入，错一个角色就错一个决策。核心类别：

```text
指针/引用：`*`/`&`/`&&` 是声明符还是二元运算符（看前一个 token 是否类型名、是否紧跟标识符）
一元/二元：`+ - * & ! ~ ++ --` 的两义；`<` 是模板尖括号还是小于号（用 CST 的模板节点兜底）
`->` / `.` / `::`：成员访问与限定名（影响 PenaltyBreakBeforeMemberAccess）
lambda：`[` 是属性、捕获列表还是下标
`const` / `noexcept` / `override` / 尾置返回：函数声明尾部
`operator` 之后的符号：`operator<` 不是模板
初始化列表 vs 块：`{}` 前一个 token 是 `=`/`,`/`(` 还是 `)`/`;`
预处理：`#define` 名字、宏参数、字符串化
```

### 4.4 换行器

不描述成"排布规则"，描述成**状态机**：

```text
每行从第一个 token 开始，维护一个栈：
  ParenState { indent, last_space, break_before_parameter, bin_pack, has_multiple_parameters, … }
对每个 token：先问 MustBreak（`{`、`;`、`//` 之类硬边界；配置里的 AlwaysBreak…）
            再问 CanBreak（空格处、逗号后、二元运算符前后、开括号后……按配置）
            每个候选断点带一个 penalty；贪心地选"还装得下且代价最小"的那条路
超出 ColumnLimit 时按 PenaltyExcessCharacter 计入并允许溢出（不是无限找最优）
```

验收方式不是"读代码觉得对"，而是 §六 的差分。

### 4.5 对齐（后处理）

```text
在"行已经断好"之后，对连续的行组做列对齐：
  AlignConsecutiveAssignments / Declarations / Bitfields / Macros / TrailingComments / Operands
  AlignArrayOfStructures（Left/Right/None）
规则：只有**相邻、同层、形态相同**的行构成一组；一行里有注释/空行/不同对齐类别的 token 就断组。
```

### 4.6 范围、禁用与 includes

```text
-lines a:b        只格式化与这些行相交的**逻辑行**（clang-format 的语义：整条逻辑行重排，
                  范围外的行一个字节都不动）
// clang-format off / on   区间内的行原样输出
DisableFormat: true        文件级关闭（格式化器直接返回原文）
SortIncludes               `#include` 块排序；IncludeCategories 分类、IncludeBlocks 分块、
                  IncludeIsMainRegex 判定"主头文件"
```

## 五、配置兼容

`clang-format -dump-config` 在 15.0.1 上打印 **136 个顶层键 + 58 个嵌套子键 = 194 行**。按前缀分组（含嵌套）：

```text
Space* 14   Align* 13   Allow* 10   Penalty* 10   Indent* 10   Break* 10   Spaces* 8
Sort* 6     Always* 4   Split* 3    EmptyLine* 2  Derive* 2    Insert* 2   Bin* 2
其余 67
```

三条实现决定：

```text
① **preset 表由 `-dump-config` 生成，不手抄**。`presets.rs` 里每个 preset 是一个
   `&[(&str, &str)]`，测试用一个脚本（`tools/gen_presets`，离线跑 clang-format）对比：
   我们的 preset 解析结果 == clang-format 的 `-dump-config`。这样 194 行的默认值不会漂移。
② YAML 用**自己写的子集解析器**，并且**遇到不认识的东西就报错**，不静默忽略。
   允许的子集：`key: scalar`、嵌套映射（BraceWrapping）、`key: [a, b]`、列表形式的映射
   （IncludeCategories）、`---` 多文档、`#` 注释、单双引号字符串。
   这条和仓库里 `.cppls.toml` 的决定同源（"给用户手写的配置一个真解析器，而不是静默忽略的
   子集"），YAML 全语法不必支持，但**不支持的部分必须说出来**。
③ 未知键与非法值：**和 clang-format 一样报错**（实测，15.0.1）：

```text
$ clang-format '-style={BogusKey: 1, IndentWidth: 4}' a.cpp
<command-line>:1:2: error: unknown key 'BogusKey'
{BogusKey: 1, IndentWidth: 4}
 ^~~~~~~~
Error parsing -style: invalid argument            （退出码 1）

$ clang-format '-style={IndentWidth: abc}' a.cpp
<command-line>:1:15: error: invalid number
```
   所以"错拼一个键"本来就是硬错误，不需要我们另立规矩：照抄这套行为（带键名、行列、退出码 1）。
   `--lenient` 只作为**扩展**存在（默认关闭），文档里标明它不是 clang-format 的行为。
④ 配置查找与继承：`-style=file` 从文件所在目录**向上**找最近的一个 `.clang-format`；
   该文件里写 `InheritParentConfig: true` 时，以**父目录**的配置为基，本文件的键覆盖它。
   注意 `InheritParentConfig` **不在 194 个键里**（`-dump-config` 里没有它）——它是加载器的
   指令，不是 `FormatStyle` 的字段；解析完就该消失，不能被当成一个样式键透传到下游。
```

preset 的实际差异（`-dump-config` 实测，抽样）：

```text
                           LLVM    Google  Chromium  Mozilla  WebKit  Microsoft  GNU
IndentWidth                2       2       2         2        4       4          2
ColumnLimit                80      80      80        80       0       120        79
BreakBeforeBraces          Attach  Attach  Attach    Mozilla  WebKit  Custom     GNU
PointerAlignment           Right   Left    Left      Left     Left    Right      Right
AccessModifierOffset       -2      -1      -1        -2       -4      -2         -2
ContinuationIndentWidth    4       4       4         2        4       4          4
BinPackParameters          true    true    false     false    true    true       true
AlignAfterOpenBracket      Align   Align   Align     Align    DontAlign Align    Align
BreakBeforeBinaryOperators None    None    None      None     All     None       All
SpaceBeforeParens          Control Control Control   Control  Control Control    Always
AllowShortFunctions…       All     All     Inline    Inline   All     None       All
DerivePointerAlignment     false   true    false     false    false   false      false
IndentCaseLabels           false   true    true      true     false   false      false
```

这张表的第一版是**错的**：生成脚本把七个 preset 塞进一个无序的 `@{}`，列头却按固定顺序打印，于是整张表错位
（把 Google 的 79 列、GNU 的 120 列写反了）。现在这张是逐 preset 单独 dump、按固定列序生成的。

## 六、精度：oracle、语料与指标

**oracle 就是 clang-format 15.0.1**，路径在本机是
`C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\Llvm\x64\bin\clang-format.exe`。
测试不去"理解"它，只对比输出。

**语料**（实测规模）：

```text
EmmyLuaCodeStyle（真实工程，自带 CLion 生成的 .clang-format）
  工程自身：198 个 C++ 文件（不含 3rd/ 与 build*）
  整个仓库：1 138 个文件 / 12.9 MB / 339 081 行（3rd/ 的第三方也在，用来做健壮性与吞吐）
  它的配置：BasedOnStyle LLVM、ColumnLimit 0、IndentWidth 4、ContinuationIndentWidth 8、
           BreakBeforeBraces Custom（AfterFunction false / AfterClass false / …）、
           PointerAlignment Right、SpacesBeforeTrailingComments 0 …
  **用它自己的配置跑 clang-format（实测，198 个文件）：165 个零变化、33 个被改动**（83% 稳定）
  —— 165 个是"稳定性样本"（格式化器动一个字节就是错），33 个是 P0 的第一批"重排样本"：
     我们必须产出和 clang-format **同样**的改动。
libstdc++ 头（模板重）：<ranges> 301 KB，clang-format 0.7 s（0.45 MB/s）——最慢的一类
工具链头（Windows SDK）、仓库自己的测试夹具（含坏代码/宏/#if）
合成用例：对每个配置键写"最小触发对"（同一段代码 × 两种配置值），由 clang-format 生成期望输出
```

**指标**（每一段都用同一套）：

```text
M1 byte-identical：输出与 clang-format 逐字节相同的文件数 / 总数
M2 差异行数：不相同的文件里，diff 行数的分布（不是只看"相同文件数"，一个文件一行差也算不同）
M3 首个分歧处：token 偏移 + 该 token 的角色 + 当时在哪个状态（ParenState 的哪个字段）
   —— 这一条决定"下一步修什么"，没有它，diff 只是一堆噪声
M4 稳定性：对 clang-format 的输出再格式化（我们的输出必须零变化）
M5 幂等：对我们自己的输出再格式化（零变化）
```

**分级门槛**（建议，按段推进）：

```text
P0 结束：M1 ≥ 95%（ColumnLimit 0 语料，即工程自己的配置上，只做空白与缩进）
P1 结束：M1 ≥ 99%（加上括号/对齐）
P2 结束：M1 ≥ 95%（换成 LLVM/Google 默认，开 ColumnLimit，即换行器上线）
P3/P4 结束：M1 ≥ 99.5%，并且 M4 = 100%
最终：M1 = 100% 于签入的 golden 语料；语料之外的新文件由差分工具随时测
```

## 七、不变量（写进类型与测试）

```text
I1 只改空白：token 序列逐 token 相同（kind+text 都相同）；注释文本逐字节相同；
   字符串/字符字面量内部的空白一个字都不改（`ReflowComments` 只碰注释的行）
I2 幂等：format(format(x)) == format(x)
I3 稳定：format(clang_format(x)) == clang_format(x)
I4 无 panic：任何输入（含坏代码、CRLF、BOM、混合 tab、非法 UTF-8 边界）都返回文本
I5 只输出合法 UTF-8 且保留 BOM 策略；行尾风格（LF/CRLF）按输入保持，除非配置指定
I6 列宽规则固定：tab 展开到下一个 TabWidth 制表位；UTF-8 按显示宽度（东亚宽字符 2 列），
   这条用"含 tab、含 CJK 注释、含 emoji"的用例对着 clang-format 校准
```

实现方式：`render.rs` 只在 `space_range` 区间替换（I1 结构性成立）；`format` 是
`&str -> String` 的纯函数（I2/I3 可无脑测）；所有输入先做"UTF-8 边界 + BOM + 行尾"归一化。

## 八、性能

基线（本机，clang-format 15.0.1）：

```text
项目代码 87 KB                        60 ms（含 ~10 ms 进程启动）→ ~1.4 MB/s
libstdc++ <ranges> 301 KB             700 ms                      → ~0.45 MB/s
97 个小文件（进程/文件 14 ms 启动）    0.4 MB / 1.4 s              → 进程模型主导
```

目标：**≥5 MB/s 单线程**（12.9 MB 语料 < 2.6 s），即 3–10× clang-format。能赢的原因是结构而不是技巧：

```text
· 没有 libclang 的 Lexer/Diagnostic 层；token 流从 rowan 的 green tree 直接取
· FormatToken 放 arena（`Vec<FormatToken>` + index），不构造 HashMap，不逐 token 分配
· 换行器是单趟状态机（O(tokens × 栈深)），不做指数搜索；对齐是单趟后处理
· render 是一次 `String::with_capacity` + 区间复制
风险点：长行（模板参数列表）里状态栈的分支会放大常数；用"最长行"用例做护栏。
```

## 九、接口

**CLI（与编辑器插件常用的 clang-format 子集一致）**

```text
cppfmt [options] [files...]
  -i, --in-place              原地改写（默认输出到 stdout）
  -style=<preset|file|{json}> 与 clang-format 同名同义；`file` 表示向上找 .clang-format
  -fallback-style=<preset>    style=file 找不到时的退路（默认 LLVM）
  -lines=<a>:<b>              只格式化这些行（可重复）
  -dump-config                打印解析后的完整配置（键名与 clang-format 一致）
  -output-replacements-xml    clang-format 的替换格式（编辑器插件用它做增量应用）
  -assume-filename=<name>     stdin 时用它决定语言与配置查找
```

**LSP**：`textDocument/formatting` 与 `textDocument/rangeFormatting` 返回单个全量 `TextEdit`
（clangd 的形态），后续可以用 `-output-replacements-xml` 的等价物返回细粒度 edit。
接入点在 `crates/cpp_ls` 的 request handler，格式化器本身不知道 LSP 存在。

## 十、分阶段计划与验收

| 段 | 内容 | 验收（M1 = byte-identical 比例） | 估量 |
|---|---|---|---|
| P0 | crate 骨架、CLI、配置子集解析 + 7 preset、token/行模型、**只排空白**（空格/换行/缩进的基本规则），ColumnLimit 0 | M1 ≥ 95%（工程配置语料） | ~1.5k 行 + 语料 harness |
| P1 | 缩进、括号换行（BreakBeforeBraces/BraceWrapping）、`AllowShort*`、对齐（AlignConsecutive*） | M1 ≥ 99%；M4 100% | ~1.5k |
| P2 | **换行器**：状态栈、惩罚表、MustBreak/CanBreak、ColumnLimit；LLVM/Google 默认配置 | M1 ≥ 95%（默认配置） | ~3–5k（最长） |
| P3 | `-lines`、`clang-format off/on`、`DisableFormat`、includes 排序、`-output-replacements-xml` | M1 不降；范围/禁用有专门用例 | ~0.5k |
| P4 | 宏：`#define` 续行（AlignEscapedNewlines）、MacroBlock、`#if` 区域、`IndentPPDirectives` | M1 ≥ 99.5%（含模板重头） | ~1k |
| P5 | 性能（≥5 MB/s）、LSP 接入、`-dump-config` 全键 golden、CI 差分任务 | 语料全跑 < 3 s；M1 = 100% 于 golden | ~0.5k |

“估量”是行数级别的相对大小，不是承诺；P2 的难度不在代码量而在**惩罚表与状态机的逐项对齐**，
它的收敛靠 §六 的 M3（首个分歧处）逐个消掉，而不是一次写完。

## 十一、风险与取舍

```text
R1 惩罚表的“长尾”。换行器的每一个决策都和别人的每一个决策耦合，改一处可能翻掉几十个文件。
   对策：M3 的"首个分歧处"按类别统计（指针/模板尖括号/二元运算符/开括号…），一次只修一类；
   每一类修完必须让 M1 单调不降（CI 里做基线比对）。
R2 版本漂移。用户机器上的 clang-format 可能是 16/17/18，输出与 15.0.1 不同。
   对策：钉版本 + `--reference-version` 说明；差分工具的 oracle 版本写进报告头；
   14→18 的差异用"每个版本的 golden"分别跑（工作量线性，不是设计问题）。
R3 许可证。**重实现行为**（读它的输出、对齐它的规则）不引入 LLVM 代码，这是本设计的路线；
   不复制 clang-format 源码。若将来要"移植某段实现"，LLVM 是 Apache-2.0 WITH LLVM-exception，
   需要在那段代码处显式标注并过一遍合规——这是产品层面的决定，写在这里以免默认。
R4 宏展开（`-f`/`Macros:`）不做。v15 的宏处理本来就是保守的；P4 只对齐 v15 的行为。
R5 Unicode 列宽/宽字符/emoji 的组合没被 clang-format 覆盖得很好，我们也以它的输出为准
   （即"和它一样"优先于"更符合直觉"）。
R6 语料的许可：EmmyLuaCodeStyle 与 libstdc++ 只作为**测试输入**在 CI 里跑，不进仓库；
   golden 语料用合成用例 + 仓库自己的测试夹具，避免把第三方代码签进来。
```

## 十三、P0a 已落地（2026-10-23，实测）

`crates/cpp_formatter` 现在能跑：`.clang-format` 子集 + 7 preset、token/trivia 模型、从语法树取角色
（模板尖括号 / 声明符 / 块括号）、行内空白规则、只替换空白的渲染器、`cppfmt` CLI、
以及 `examples/spacing_audit.rs`（对着 clang-format 逐 gap 比）。

**做什么、不做什么。** P0a 只重写**同一行内、且不含注释的** gap；换行、缩进、注释周围的一切原样复制。
于是"一次只动一个变量"：行结构的问题留给 layout（P0b），这里量的是空白规则本身。

**指标与数字**（`spacing_audit`：同一文件分别用我们和 clang-format 格式化，逐 token 对 gap 宽度，
两边都在同一行且无注释的 gap 才计入）：

```text
工程自身 106 个文件 / 58 156 个 gap      **99.71%** 相同（57 985 / 58 156）
整个仓库 774 个文件（含 3rd）/ 632 565    98.8%
```

剩下的差距按成因分成四类，前三类是**已知的下一段**，不是缺陷：

```text
· `AlignArrayOfStructures: Left`（工程配置开着）：`{6,  6 }` 这种列对齐 —— P1 的对齐后处理
· `SortIncludes`：clang-format 会重排 `#include`，token 顺序因此不同 —— P3（审计里这几份文件被跳过）
· `#` 行与宏体：P0a 原样复制，clang-format 会规范化 `#  define` 和宏参数里的逗号 —— P4
· 少量 token 角色判错（`>`/`<` 在解析失败的代码里、函数指针的 `(`）—— 逐条消
```

**两个在这一段里被抓到的真实缺陷**，都值得记住：

```text
① preset 生成器的键名正则 `^[A-Za-z_]+` 丢掉了 `Cpp11BracedListStyle` 和
   `SpaceBeforeCpp11BracedList`（键名里有数字）→ 工程自己的 `.clang-format` 解析失败 →
   审计静默回退到 LLVM 默认值，整轮数字都建立在一个错的配置上。现在正则允许数字，并且
   审计在配置解析失败时会把它打印出来（不再静默回退）。
② 删掉一个空格可能把两个 token 粘成一个：`> >` → `>>`（右移）、`+ +` → `++`、`/ /` → 注释。
   这不是规则能避免的，是词法的性质。所以渲染器最后**重新词法**并比对 token 序列，不一致就
   原样返回整个文件——"拒绝格式化"是格式化器唯一诚实的失败方式，I1 由此由构造保证。
   实测触发：`LuaParseError.h`。
```

**测试**：22 个单元测试（配置、preset、token/gap、角色、空白规则、渲染、token 保全、幂等）；
`spacing_audit` 作为可重复的测量工具。
## 十二、与现有仓库的关系

```text
· 新 crate `crates/cpp_formatter`，`crates/*` 已被 workspace 收进来，无需改根 Cargo.toml 的 members
· 只依赖 `cpp_parser`（lossless CST、lexer、trivia）与 `clap`；不依赖 `cpp_code_analysis`
· 复用 `cpp_parser` 的测试夹具作为"坏代码/宏/`#if`"用例来源
· 格式化是文本层：不读 cooked 读数、不碰 `ReadingOffset`、不写摘要缓存
· 接入 LSP 时只加一个 handler（`textDocument/formatting`），格式化器本身不知道 LSP 存在
```

---

## 附：这件事的最小验证（在写 P0 之前）

```text
1. 用 clang-format 跑一遍 1 138 个文件（工程自己的配置），把"变了的 14% 文件"存成
   `corpus/expected/`；这是 P0 的先行基线，也是 M3 的第一批样本。
2. 手写 30 个"最小触发对"（同一段代码 × 两种配置），确认我们对这些键的理解与
   clang-format 一致（每个用例两行：配置 → 输出）。
3. 量一遍 render 的形态：把源文本按 token 切成 (token, space) 对，再拼回去，断言
   == 原文（这是 I1 的实现是否正确的第一道检查，先于任何格式化逻辑）。
```

## 十四、P0b：整文件对比与"操作符不能被拆开"（2026-10-23，实测）

P0a 只测**同一行内的 gap 宽度**，看不到"行结构/缩进"的差异。P0b 起用一个整文件审计
`examples/layout_audit.rs`：同一个文件两边各格式化一次，逐字节比，并给出**第一处不同的行**的分类
（缩进 / 断行 / 空行 / 其它）。这比"相同文件数"更有用——一个文件只要改对那一类，它就从"不同"变成"相同"。

**oracle 换成 clang-format 22.1.3**（见 §六）；语料是工程自身的 43 个 C++ 文件
（`LuaParser` + `CodeFormat` + `Common`，工程自带 `.clang-format`，ColumnLimit 0）。

```text
起点（P0a 之后）   28 / 43 逐字节相同（65.12%）
                   分类：缩进 1、空行 3、其它 8、跳过 3（token 流不同）

现在               30 / 43（69.77%）
                   分类：缩进 2、空行 3、其它 5、跳过 3
```

**改对的那一条：lexer 拆开的多字符操作符仍然是同一个操作符。**
`cpp_parser` 的词法把 `>=` 拆成 `Greater` + `Assign`、`&&` 拆成两个 `Ampersand`（parser 自己按上下文再拼回去）。
只按 token kind 看的空白规则于是把两个"二元运算符"分开，写出 `if (end > = start)` 和 `char * *argv`。
判据来自源文本：**一个字节都没有的 gap 不是谁写空格的地方**，而这类相邻对在 clang-format 眼里是一个操作符，
所以保持贴合；这条同时修掉了审计里被标为"token 流不同"的两个文件，以及 `LuaSyntaxNode.cpp` / `LuaSource.cpp`
里的 `> =` 行。

**试过并回退的：整行重排（缩进 + 空行折叠）。**
按块括号层级算缩进的想法在语料上从 30 掉到 **1 / 43**，分类里 34 个是缩进——原因很具体：
**`namespace` 的花括号不增加缩进**（`NamespaceIndentation: None` 是默认值），而这份语料的代码到处是
`namespace X {`。所以 P0b 的缩进必须先把"这个 `{` 是 namespace / class / 语句块"分开，而不是只数括号层数。
这次尝试已完整回退（`render.rs`/`lib.rs` 回到 P0a 状态），没有留下半成品。

**剩下的 13 个文件由谁负责**（审计样本给的分类）：

```text
AlignArrayOfStructures: Left       {"and",      TK_AND     },      P1 对齐后处理
AlignTrailingComments              TK_GLOBAL,// global            P1 对齐后处理
std::function<bool (T)> 的空格      vs  bool(T)                    角色规则（P0b 内可单独修）
块注释续行的缩进                                                     P0b，且要有 namespace 规则之后
空行折叠到 MaxEmptyLinesToKeep                                      P0b，和缩进同一趟
```

**关于 trailing comment 的一个教训**（写下来省一次弯路）：把 `SpacesBeforeTrailingComments: 0` 实现成
"注释前固定 0 个空格"让数字从 30 掉到 28——因为同一份配置还开着 `AlignTrailingComments`，
注释的列是**对齐器**决定的，不是单点空格数。它属于 P1，不属于 P0b。
docs: what the indentation pass learned, and why it is not wired in yet

A second attempt at P0b's indentation, this time with the rule the first attempt was missing
(`namespace` bodies do not indent), and with `layout.rs` written and tested in isolation — five tests: a function
body indents, a namespace body does not, an access specifier sits at its offset, a braced initializer does not move
the level, a continuation line is left alone.

Wired into the renderer it took whole-file agreement from **30 of 43 to 16 of 43**, and the audit says why: of the 27
differing files, **18 diverge at a continuation line**, and those lines are exactly the ones the new pass declines to
decide. clang-format re-indents them — a constructor initializer list to `ContinuationIndentWidth`, a chained call to
the open parenthesis under `AlignAfterOpenBracket: Align` — while the pass copies the source's columns. So on a file
whose line structure clang-format would change, half-right is worse than honest: leaving indentation alone happened
to preserve the files the source had already formatted.

Two smaller things were measured and are worth keeping:

* `case` labels: dropping a level (`IndentCaseLabels: false` read as "outdent") put them at 4 where clang-format has
  8 — on this corpus the label sits at the body's level. Fixed in the draft; it changed no file's verdict, which says
  the case-label rule is not where the remaining error is.
* the indentation of a line inside a multi-line block comment, and of a macro body's continuation, are both this
  pass's to decide or neither's — the audit's samples include `/* ORDER OPR */` at 2 where clang-format has 8.

**State:** `layout.rs` is reverted along with the renderer change (the repository does not carry an unwired module —
see the note in §十四 about the same decision for the earlier draft). The next attempt starts where this one stopped:
decide continuation lines in the same pass, from `ContinuationIndentWidth` / `AlignAfterOpenBracket`, because they
are 18 of the 27 remaining files.
docs: the indentation pass, second attempt — three mechanisms it needs and the order to build them in

The draft was written and tested twice now (`layout.rs`, five to eight tests each time), wired into the renderer both
times, and reverted both times. What the measurements say, in the order the code has to be built:

```text
attempt 1   level from block braces only                         30/43 → 1/43
            the rule missing: namespace bodies do not indent (NamespaceIndentation: None)
attempt 2   + namespace rule, + access specifiers, + directives,
            + continuations (Align / ContinuationIndentWidth),
            + case labels at the body's level, + case statements one deeper      30/43 → 16/43
            still 18 of 27 differing files diverge on a first indent
```

Three mechanisms were found by looking at those 18, and each is a real defect in the draft rather than a missing
feature:

```text
① a switch body`s `{` is not in `roles.block_brace`   — every `case` label sat one level short. The fix is to treat
   "not an initializer list" as a block (`init_brace` is the exception that is actually marked), and it moves the
   label from 4 to 8, which is what clang-format has on this corpus.
② a function body inside a namespace scanned back to the `namespace` keyword and was classified as a namespace body,
   so the whole body was one level short. The fix is to stop that scan at a `)` — a parameter list ends the question.
③ a `case` label`s statements are one level in, and the block`s `}` is not — the fix is to add the case level only
   for lines that are not a block closer.
```

With ① and ③ applied the layout tests pass on the shapes they describe (`case` at 4, `break` at 6 in LLVM's
IndentWidth 2), and the whole-file audit does **not** move: still 16/43. That is the honest state — the indentation
rules are right on their fixtures and the corpus still prefers the version that does not touch indentation, because
every file whose *line breaks* clang-format would change also needs the breaks decided, and at `ColumnLimit: 0` the
break decisions are the aligner's (P1), not the indenter's.

**State:** reverted again — `render.rs` and `lib.rs` are at the committed state, `layout.rs` is not in the tree, the
audit is 30/43 and 26 tests pass. The next attempt should come **after** P1's aligner, not before it: the columns a
continuation line needs are the columns the aligner produces.
docs: the trailing-comment aligner, measured, and the two numbers that sent it back

Built `align.rs` — a post-pass computing, for each trailing comment, the column its *run* agrees on: replay what the
renderer is about to emit (the source's line breaks and indentation, `space::between`'s gaps), take the widest code
end in a run of consecutive lines, add `SpacesBeforeTrailingComments`. Five tests; the whole-file audit says:

```text
no aligner                                      30 / 43
runs of two or more aligned                     29 / 43
every trailing comment (singletons too)         28 / 43
```

So it is not kept, and the reason is a measurement rather than a bug in the idea: the corpus's configuration has
`SpacesBeforeTrailingComments: 0` and no `AlignTrailingComments` key (clang-format's default), and on the files where
the aligner changed something it mostly changed the **wrong** thing — `TK_GLOBAL, // global` became `TK_GLOBAL,//` on
one file while other files' one-space comments stayed as they were and are what clang-format produces. That is the
signature of a simulation whose columns are not the emitted columns: `SpacesInLineCommentPrefix`, tabs, and CJK text
all move a column, and this replay counts `chars`.

**What the next attempt must do first**: make the column model one shared thing — display width (tabs to the next
tab stop, East-Asian wide = 2, as `docs/cpp-format.md` §七 I6 requires), used by the aligner *and* by the indenter —
because the indenter's continuation lines (18 of 27 remaining files) need exactly the same numbers. Two independent
column models is how both attempts ended up worse than doing nothing.

**State:** reverted; `align.rs` and `layout.rs` are not in the tree, the audit is 30/43, 26 tests pass.
docs: the column model is right, the aligner is still wrong — clang-format 22's `AlignTrailingComments` is a struct

Third measurement of the trailing-comment aligner, this time on top of a proper column model
(`columns.rs`: tabs to the next `TabWidth` stop, East Asian wide = 2, combining marks = 0 — six tests), so the
arithmetic that the first version got wrong is not the explanation any more:

```text
no aligner                                      30 / 43
aligner on `char` columns                       29 / 43   (round 1)
aligner on the column model                     28 / 43   (round 2, with singletons aligned too)
```

**What the oracle's own dump says**, and it changes the shape of the fix:

```text
$ clang-format -style=file -dump-config … | grep -A4 AlignTrailingComments
AlignTrailingComments:
  AlignPPAndNotPP: true
  Kind:            Always
  OverEmptyLines:  0
SpacesBeforeTrailingComments: 0
SpacesInLineCommentPrefix:
  Minimum:         1
```

So on `22.1.3` the option is a **struct** (`Kind`, `OverEmptyLines`, `AlignPPAndNotPP`), not the `bool` our
`presets.rs` carries — the preset table was generated from 15.0.1, where it was a bool. `Kind: Always` means
alignment *is* on, so the behaviour is not "no alignment"; what is still missing is clang-format's **group-breaking
rule** (`WhitespaceManager::alignTrailingComments` splits a run when the next comment's column exceeds the current
maximum by more than the alignment allows, and it never aligns across a line without a comment or across
`OverEmptyLines` empty lines). Building "align every run" without that rule moves comments clang-format leaves alone,
which is exactly the -2 files.

**Two things this round establishes for the next one:**

1. `presets.rs` is 15.0.1's, and `AlignTrailingComments` is one of the keys whose *type* changed. The preset generator
   has to be re-run against 22.1.3 and the key parser taught the struct form (`AlignTrailingComments: {Kind: …}`),
   otherwise every alignment option is read as a bool that no longer exists.
2. The column model is now written and measured (it was the shared prerequisite for both the aligner and the
   indenter); it is **not committed** because nothing in the tree consumes it yet — an unwired module is what this
   repository has decided not to keep (§十四, §十六).

**State:** reverted to 30/43, 26 tests, working tree clean.
## 十九、presets 按 22.1.3 重生成：268 个键，旧键仍被接受（30/43 保持）

上一轮我误判了：以为 22.1.3 的 `-dump-config` "值整体错位"。**是错的判断，已经在下面更正。** 实测
（把可疑的值写进配置再 dump 回来）：

```text
$ cat .clang-format
Cpp11BracedListStyle: AlignFirstComment
BinPackParameters: BinPack
ReflowComments: Always
AlignAfterOpenBracket: false

$ clang-format -style=file -dump-config a.cpp
AlignAfterOpenBracket: false        ← 回显
BinPackParameters: BinPack          ← 回显
Cpp11BracedListStyle: AlignFirstComment   ← 回显
ReflowComments:  Always             ← 回显
```

这些是 **22 里真实的新拼写/新类型**，不是错位：

```text
AlignAfterOpenBracket     bool（true = 旧 Align，false = 旧 DontAlign）
BinPackParameters         枚举（BinPack / OnePerLine）
ReflowComments            枚举（Never / IndentOnly / Always）
Cpp11BracedListStyle      枚举（含 AlignFirstComment）
SortIncludes              22 的 dump 里不再出现
```

所以表按 22.1.3 生成：**268 个键**（15 时代是 188），`options.rs` 同步（268 个变体）。

**一个关键发现：22 仍然接受、但不再 dump 的键有 38 个。** 语料自己的 `.clang-format` 第 8 行就是
`AllowAllConstructorInitializersOnNextLine` —— 我们的加载器因此拒绝了整份配置，审计**静默回退到 LLVM 默认值**，
数字从 30 掉到 27。这是"配置没生效"而不是"规则变差"：

```text
拒绝语料配置 → LLVM 默认   27 / 43   （SpaceBeforeRangeBasedForLoopColon 变回 true，
                                      于是输出 `for (auto pattern : patterns)`）
接受旧键        → 语料配置  30 / 43
```

修法：`presets::PARSED_BUT_NOT_DUMPED`（38 个键，含 `AllowAllConstructorInitializersOnNextLine`、
`SpacesInParentheses`、`SortIncludes`、`BraceWrapping` 等容器）——**接受但暂不映射**：`SpacesInParentheses`
实际是 `SpacesInParens`，`SortIncludes` 已是枚举，宏列表搬到了 `Macros` 之类的新键。接受它让文档可读，
映射它另算一步；在那之前，用旧键的风格会得到新键的 22 默认值。

**结果**：整文件一致 **30 / 43（69.77%）**，与使用 15 表时相同，但底层是**正确的 22 键集与新类型**；
分类仍是 indent 2、空行 3、other 5。另外记一条审计自身的教训：`style_for` 的回退必须**显式打印**
（它确实打印了，`fallback` 那行就是找到问题的入口），否则"配置没生效"会被误读成"规则退步"。## 二十一、P0b 缩进落地：30/43 → 32/43（合并回 main）

分支 `formatter-layout-p0b` 已合并（`67a0f15`）。下面的每一步都是整文件审计量出来的，退步都在分支上被回退过。

```text
起点（只有空白规则）            30 / 43
接上 layout（含 namespace/switch/case 规则）  17 → 19
ctor 初始化列表的列               19 → 25
标签深度改成"层"而非开关          25 → 28
语句层级取代花括号深度 + goto 标签  28 → 29
花括号内部重置括号上下文（lambda）  29 → 29
access specifier 的空行规则        29 → 32   ← 越过基线
现在：32 / 43（74.42%），indent 4、空行 0、other 4、跳过 3
```

**这轮抓到并修正的机制**（都是被真实文件的偏差逼出来的，不是设计时想到的）：

```text
① 层级要按"语句缩进层级"记，不是花括号深度：case 体里再开 switch 时，内层标签要累计（12→16）
② 行级水平只在**行首 token** 更新：同一行每个 token 都覆盖会让 `case X: {` 行末括号记错层
③ 花括号打开时重置括号上下文：lambda 体在 `f(` 里，但它是块缩进而不是对齐到括号
④ goto 标签语义用 oracle 实测：IndentGotoLabels=true → 标签在**开块那一行**的层级；false → 第 0 列
⑤ `EmptyLineBeforeAccessModifier: LogicalBlock`（默认）+ `EmptyLineAfterAccessModifier: Never`：
   `private:` 前补一个空行、其后不补 —— 这条一次消掉 3 个空行差异
```

**两次失败并回退的尝试**（记录以免重复走）：

```text
· 注释行缩进：注释 gap 需要按"注释前/注释后"两段分别计新行；两次重做分别得到 22/43 和 23/43，
  差异来自空行重复计数（mantra：gap 里的每个字节只能写一次）。仍在待办里。
· 行首 `{` 的初始化列表元素：需要把"列表元素"也当作续行（ContinuationIndentWidth），未做。
```

**剩余 11 个文件**：indent 4（`<<` 链操作数对齐、行首块注释、初始化列表元素、lambda 体层级）、
other 4、跳过 3（token 流不同，属 include 排序/宏展开，P3/P4）。
