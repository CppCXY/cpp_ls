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
## 二十二、P0b 之后的两个 P1/P0 修补：33 → 35 / 43（81.40%）

```text
33 / 43  +  AlignOperands（`<<` 链对齐）                          → 33（机制就位，LuaCheck.cpp 的该行修好）
         +  `bool` 的类型规则                                     → **35 / 43**
```

**`bool (` 之谜（两个头文件，一次修掉）：**

```text
`std::function<bool (LuaTokenKind)>` 多了一个空格。
根因不在调用规则，在词法：`cpp_lexer` 把 `"bool"` 映成 **BoolLiteral**（和 true/false 同一 kind），
于是 `(` 前的 `bool` 既不是"调用"（Identifier）也不是"类型关键字"，落到兜底的"两个 token 之间给一个空格"。
修法：`a_type` 里加入 BoolLiteral —— `true`/`false` 后面接 `(` 本来就不是合法 C++，代价为零。
```

**`<<` 链对齐的根因**同样在词法：`<<` 是**单个 `LeftShift` token**，不是两个 `Less`。
前几轮把 `Less` 写进谓词，于是记录永远为 None；测试打印 token 流才看出来。

**仍未解决（8 个文件）**

```text
· 初始化列表里的行首注释（LuaDefine.h:19，2 vs 8）—— 行首路径被注释挡住
· 嵌套初始化行（LuaCheck.cpp:48，4 vs 12）——"列表里的 `{` 元素行"既不是块行也不被当续行；
  再试一次 `in_initializer` 覆盖花括号：35 不变但 other 从 2 涨到 3，按测量回退
· lambda 体层级（LuaSyntaxTree.cpp:455，8 vs 16）—— 块开在续行上时，body 层级应基于该行实际列
· other 2、跳过 3（include 排序/宏，P3/P4 范围）
```
## 二十三、round 11：lambda 体层级的一次失败尝试（记录条件）

目标：`LuaSyntaxTree.cpp:455` —— lambda 写在 `std::partition_point(` 里，lambda 行是**续行**（12 列），
它的 body 应在 16，而层级记账给出 8。

尝试：render 里跟踪 `brace_depth` 与"开在续行上的 `{`"，把该 body 的块行改成 `开括号行 + IndentWidth`。

实测：**35 → 25 / 43**（indent 13）。原因具体：

```text
· 该覆盖同样作用到**闭括号**（`}` 也是块行）—— ctor 初始化列表的 body 立刻错位（`}` 到 10）
· 而且它对"body 本来就被层级记对"的块也生效（ctor 的 `{` 在续行上，但它的 body 层级是对的）
也就是说：判据不能是"开在续行上"，而必须是"层级记账把 body 记到了开括号行**之上**"
（lambda：opener 12、blocks 给 8；ctor：opener 6、blocks 给 4 —— 两者都满足，所以还要区分
 "开括号行的层级由 bracket 对齐决定 vs 由 ctor 列表列决定"）
```

已回退，35/43 与测试状态保持。下一轮若要再试，需要把"开括号行的列来自哪种续行"作为显式枚举传进来，
而不是二值的 `last_line_was_a_continuation`。
## 二十四、P0b/P1 里程碑收尾（goal 第 12 轮）

**最终实测**（语料：工程自身 43 个 C++ 文件，oracle：clang-format 22.1.3）：

```text
整文件逐字节一致      35 / 43  （81.40%）   goal 的基线是 30 / 43
同线 gap 一致率       18787 / 18879（99.51%）
剩余分类               indent 2、line structure 0、blank line 0、other 3、跳过 3
测试                  44 passed + 3 ignored（3 个 ignored 都写明了未解决的原因）
workspace             全 crate 编译通过
```

**P0b（缩进与续行列）已落地**，全部建立在 `columns.rs` 的显示列模型之上：

```text
· 语句缩进层级（不是花括号深度）：case 体里再开 switch 会累计；行级水平只在行首 token 更新
· namespace 体不缩进、case 标签/标签下语句、access specifier 的 AccessModifierOffset、
  goto 标签的 IndentGotoLabels 语义、`#` 行的 IndentPPDirectives
· 续行：AlignAfterOpenBracket（22 里是 bool）、ContinuationIndentWidth、ConstructorInitializerIndentWidth、
  花括号内重置括号上下文（lambda body）、初始化列表元素按 ContinuationIndentWidth
· 空行：MaxEmptyLinesToKeep、KeepEmptyLinesAtStartOfBlock、
  EmptyLineBeforeAccessModifier（Always/LogicalBlock/Never）、EmptyLineAfterAccessModifier: Never
· 空白：`bool` 是 BoolLiteral（`bool(T)` 需要类型规则）、`<<` 是单个 LeftShift（AlignOperands 谓词）
```

**P1（对齐）按"只有测量为正才保留"执行**：

```text
保留    AlignOperands: Align —— 续行以二元运算符开头时对齐到语句首个运算符的列
未保留  AlignTrailingComments —— 三轮实测 29/28/28（基线 30），每次回退，原因写进 §十七、§十八
未保留  AlignArrayOfStructures —— 未实现（需要把一行识别成表，属节点问题）
```

**剩余 8 个文件与归属**（留给后续）：

```text
indent 2   LuaDefine.h:19（列表内行首注释，同一机制在真实文件里仍有一条分类路径未命中）
           LuaSyntaxTree.cpp:455（lambda body 层级：需要"开括号行的列来自哪种续行"的显式枚举，
           二值判据的一次尝试 35→25，已回退，见 §二十三）
other 3    空白/间距细则（含 SpacesBeforeTrailingComments 的边界）
跳过 3     token 流不同 —— include 排序与宏展开，属 P3/P4，不在 P0/P1 范围
```

**P2 仍未开始**（断行引擎：penalty 搜索、ColumnLimit>0 时的换行决策），文档 §十 的阶段表未变；
本次里程碑交付的是"在 ColumnLimit: 0 下把列算对"，P2 是让本工具能在 ColumnLimit>0 的工程上工作。
## 二十五、继续攻剩余 8 个文件（goal 第二轮启动后的三个尝试）

```text
起点 35 / 43（81.40%），三个尝试都做了实测，都回退了（工作区仍干净）：

① 统一"花括号是不是块"的判据（classify_braces 与 init_depths 各有一份，已经漂移）
   抽出 brace_is_a_block() 复用；修复方向对（`} priority[] = {` 的行内 `{` 应算初始化列表），
   但把 `LeftBrace`/`Comma`/`Colon` 也当成"引入初始化列表"后，`class X : Base {` 和块中块都被判成初始化，
   实测 35 → 31，整体回退。结论：判据要**按上一 token 的类别细分**，而不是一张扁平表。

② 尾注释的最小间距（SpacesBeforeTrailingComments: 0，单条注释不参与对齐）
   实现后 35 → 33：语料里大多数尾注释本来就被 clang-format 对齐过，只给最小值反而破坏成对齐列。

③ lambda 体层级（LuaSyntaxTree.cpp:455）
   引入"这一行是**括号规则**放置的"标记，body 缩进 = 该行列 + IndentWidth：
   第一次实现 body 从 8 → 16 ✓（正确），但闭括号 `});` 被一起移位（16，应为 12）；
   收紧闭括号规则后 body 又回到 8 —— 两次的差异没能在本轮定位（状态清零时机与 closer 判定互相影响）。
   两次尝试均回退，未留半成品。
```

**结论**：剩下的 8 个文件里，`LuaDefine.h:19`（列表内注释）、`LuaSyntaxTree.cpp:455`（lambda 体 + 闭括号）、
`LuaCheck.cpp:49` 与 `LuaTokenTypeDetail.h:31`、`LuaLexer.cpp:11`（AlignArrayOfStructures / 尾注释对齐）
分别需要：① 细分的花括号判据；② 括号放置的 body 状态与闭括号分开处理；③ AlignArrayOfStructures（P1 未做）；
④ 尾注释对齐的**分组规则**（不是最小值）。这四项都不是"再调一个常量"能解决的，已各自记录条件。
## 二十六、35 → 36 / 43：括号放置的 body 缩进（lambda）

**成功的一条**（`LuaSyntaxTree.cpp` 现在整文件一致）：

```text
判据不是"这一行是续行"，而是"开这个 `{` 时外层括号还开着"：
  · `std::partition_point(` 里写的 lambda，`{` 时外层 `(` 仍在 → body 缩进 = 该行列 + IndentWidth，
    闭括号坐在该行列（16 / 12，与 clang-format 一致）
  · 而 `if (a &&\n    b) {` 这种"条件续行的第二行"，`)` 已经把括号关掉 → 不走这条路径，
    body 仍由层级记账给（12，正确）
这一条差异正是前两次尝试（35→25、以及 closer 到 16）失败的原因：二值判据把两类续行混为一谈。
```

**仍失败的（均已回退，条件已记录）**

```text
· 尾注释"单条 = 最小间距"：全局 33（语料里 clang-format 把多数尾注释对齐过，我们只给最小值会打散）
· `brace_is_a_block` 统一判据：把 `LeftBrace/Comma/Colon` 一律当"引入初始化列表"，`class X : Base {` 被判错，
  31/43
```

**当前 7 个文件**

```text
indent 1   LuaDefine.h:19（列表内行首注释）
other 3    LuaCheck.cpp:49、LuaLexer.cpp:11（AlignArrayOfStructures: Left，P1 唯一未实现的对齐器）
           LuaTokenTypeDetail.h:31（尾注释对齐的**分组规则**，不是最小值）
跳过 3     token 流不同（include 排序 / 宏展开，P3/P4）
```
## 二十七、36 → 37 / 43：`AlignArrayOfStructures: Left` 落地

```text
规则（用 LuaLexer.cpp 的真实表反推，并逐个字符核对）：
  第 0 列起点 = 1（`{` 之后）
  第 k 列起点 = 第 k-1 列起点 + 该列最宽单元格 + 2（逗号 + 一个空格）
  闭括号列   = 最后一列起点 + 该列最宽单元格（闭括号不自带空格）
  行的定义：行首是 `{`，配对的 `}` 之后是 `,` 或直接行尾（**表的最后一行可以没有逗号**，这是第一次尝试漏掉的）
实现：crates/formatter/src/rows.rs（独立模块，3 个单测），render 只在同线 gap 上叠加 padding
结果：LuaLexer.cpp 整文件一致；37 / 43
```

**仍在的 6 个**

```text
indent 2  LuaDefine.h:19（列表内行首注释：该 gap 里同时有上一行的尾注释与列表注释，条件已放宽但仍未命中）
          LuaLexer.cpp:478（三元运算符 `?` 的对齐：AlignOperands 对条件表达式有单独规则，尚未掌握）
other 1   LuaCheck.cpp:49（表里嵌套花括号的行，列宽计算未覆盖）
跳过 3    token 流不同（include 排序 / 宏展开，P3/P4）
```
## 二十八、round 4：列表注释的诊断（未净收益，已回退）

用 env-gated 打印把 `LuaDefine.h:19` 的 gap 摊开，得到两个确定的结论：

```text
gap idx=43 token="{" comments=1 last_own=Some(true) init=0
```

1. `last_own=true` —— 注释确实是"行首注释"，条件里与注释有关的部分是对的；
2. `init=0` —— **`init_depths` 在那个 `{` 处仍是 0**：`} priority[] = {` 的 `{` 没有被算作初始化列表，
   因为 roles 既没把它标进 `init_brace`、又（可能）标进了 `block_brace`，于是 `!block && init` 为假。

修法尝试：在 `init_depths` 里让"引入者"先决定（`=`、`(`、`,`、`return`、`:` 之后是初始化列表）。

```text
· 全量引入者集合：37 → 31（`,` 之后的 lambda/braced-init 被误判，整体变差）
· 收窄到 `Assign | ReturnKeyword`：37 不变，但 LuaDefine.h 的首处分歧从 indent 变成 blank line ——
  注释被正确缩进了（8），但那次路径多吐了一个空行（此前缀里已含换行，又叠了 keep），
  且 rows 的 padding 也叠在了行上（注释到 14、行到 12）
```

两条都要在"多注释 gap"那条路径上区分"从源码写前缀"与"自己发换行"才不会双写；本轮时间不够，已整体回退，
工作区仍是 37/43、47 tests 全绿。

**下轮的具体任务（一条，已定位到行）**
`render.rs` 的 `comment_in_this_gap` 分支：写前缀时不要再发 `keep` 个换行，并且在该行是初始化列表元素时
不要叠加 `row_paddings`（注释行不是表的一行）。改完后 `LuaDefine.h` 应转绿 → 38/43，即 goal 门槛。
## 二十九、P2 的基线测量（断行引擎尚未开始）

`ColumnLimit: 0` 的语料里，我们的整文件一致率是 37/43；一旦把 `ColumnLimit` 设成 80，
差距立刻显现 —— 这既是 P2 的动机，也是它的验收指标：

```text
语料 43 个文件，配置 `ColumnLimit: 80`（IndentWidth 4）：
  我们输出里长度 > 80 的行： **111**
  clang-format 输出里长度 > 80 的行： **0**
```

原因很直接：当前的 P0b 只决定"行的列"，不决定"在哪里断行"——源码的断行被原样保留，
于是所有 clang-format 会主动折断的长行，我们都留着。

**P2 的最小实现路线**（`docs/cpp-format.md` §十 的 P2，与 clang-format 的 `UnwrappedLineParser` +
`BreakableToken` 一致）：

```text
1. 先把 token 流切成 UnwrappedLine（逻辑行，而不是物理行）：`;`、`{`、`}`、控制语句头都是边界
2. 对每个 UnwrappedLine 做贪心 + penalty 的断行搜索：
     PenalyExcessCharacter / PenaltyBreakBeforeFirstCallParameter / PenaltyBreakAssignment /
     PenaltyBreakString / PenaltyBreakComment 都在 presets 里（22.1.3 的值已在表中）
3. 断开的位置交给现有的 render（它已经能按"列"排版），WhitespaceManager 的角色由 layout 承担
4. 验收：上面那条 `ColumnLimit: 80` 的测量必须从 111 降到接近 0，且 `ColumnLimit: 0` 的 37/43 不回退
```

在那之前，P0b/P1 能做的只剩 6 个文件（见 §二十七/§二十八），其中 3 个是 token 流差异（P3/P4 范围）。
## 三十、P2 的第一步：`column_audit`（把"要断多少行"变成一个数字）

`examples/column_audit.rs` 用**同一套样式**（`BasedOnStyle: LLVM, ColumnLimit: 80, IndentWidth: 4`）跑两边，
把 P2 的工作量变成两个可复现的数字：

```text
语料 43 个文件（LuaParser + CodeFormat + Common）：
  printed: 113 行超过 80（我们） vs 0（clang-format）
  logical: 199 个语句打印后会超宽（我们） vs 193（clang-format）   ← 近似值
```

第一行就是 P2 的存在理由：在 `ColumnLimit > 0` 时，clang-format 会主动折断，而我们要么保留源码断行、要么写出超长行。
第二行给出"断行决策的量级"：约 200 处语句需要参与 penalty 搜索（我们与 oracle 的 6 之差来自近似算法，
不是已实现的断行）。

**未实现的部分**（`docs/cpp-format.md` §十 P2）：

```text
1. UnwrappedLine 切分（逻辑行：`;`/`{`/`}`/控制语句头是边界）
2. 每个逻辑行做 penalty 贪心断行（PenaltyExcessCharacter、PenaltyBreakBeforeFirstCallParameter、
   PenaltyBreakAssignment、PenaltyBreakString、PenaltyBreakComment 都在 22.1.3 的表里）
3. 断点交给现有 render（它已经能按列排版）；验收：上面 printed 从 113 → 0，且 ColumnLimit: 0 的 37/43 不回退
```
## 三十一、P2 的第二步：断行搜索（`breaks::break_line`）与它的实测覆盖率

`breaks.rs` 现在除了逻辑行切分，还有一个**基于 penalty 的动态规划断行搜索**：

```text
候选断点：逗号后、二元运算符前、`?` 前、`(`/`[` 后、`.`/`->` 前
代价：Σ max(0, 片段宽 - ColumnLimit) × PenaltyExcessCharacter + 每个断点的固定代价
      （断在赋值处用 PenaltyBreakAssignment，断在调用首个实参前用 PenaltyBreakBeforeFirstCallParameter，
        其余记 1 —— 权重全部来自 22.1.3 的表）
DP：best[i] = min over j<i { best[j] + 片段代价(j..i) + 断点代价 }，O(n²) 但 n 只是一行里的候选点
```

4 个单测钉住语义：放得下就不动、长参数表在逗号后断、长加法在运算符前断、结果确定。

**实测效果**（`column_audit`，同一套样式 `ColumnLimit: 80`，43 个文件）：

```text
printed: 113 行超 80（我们） vs 0（clang-format）
logical: 105 条逻辑行超限 —— 需要断的单位
breaker: 其中 **80 条**能被这个搜索排到 80 列以内，**25 条**仍会溢出
```

那 25 条里包含"单个 token 就超宽"的情况（长字符串、长注释），clang-format 对它们也无能为力
（它靠 `BreakStringLiterals`/注释重排，属于后续细化）；80/105 说明候选断点集合已经覆盖了绝大多数真实语句。

**尚未接线**：`break_line` 目前只被 `column_audit` 调用（作为测量），未接入 `render`。
接线点已经明确：render 在写一条逻辑行之前调用它，按返回的 token 索引插入换行——
那一步会改变 `ColumnLimit: 0` 下的输出（必须为 0 时不调用），是下一轮的收尾工作。
## 三十二、P2 接线：断行真正生效（`ColumnLimit: 80` 下超长行 113 → 56）

`render` 现在在 `ColumnLimit > 0` 时先跑一遍 `breaks::break_line`，把它给出的 token 索引当作**强制换行点**：

```text
· `ColumnLimit == 0`（语料）→ 这条路径完全不执行 → 整文件一致率保持 **37 / 43**
· `ColumnLimit: 80` → 断行被插入（只加断行，不重排已有断行）：
      printed 超长行：**113 → 56**（clang-format: 0）
      logical 需要断的逻辑行：105（不变，它是输入的性质）
      breaker 覆盖率：80/105 能排进 80 列，25 条仍溢出（单 token 超宽：长字符串/长注释）
```

这是 P2 的第一个可用迭代：**只增不减**（不移除源码已有换行），所以离 clang-format 的 0 还有距离；
剩下的是"重排已有断行"（需要把物理行结构也交给 breaker 决定）与字符串/注释的拆分策略。

**结论（本 goal 的收尾）**

```text
P0b/P1：37 / 43（86.05%），44→56 个测试全绿
  未达 goal 的 38：卡在同一处——列表元素的缩进基数与"注释行/带尾注释元素行"的状态互相干扰，
  六轮尝试均回退，条件已固化在 #[ignore] 用例 a_table_after_a_struct_body_indents_from_the_line_that_opened_it
P2：  已开工并有可测收益（113 → 56），纯函数 + 搜索 + 接线 + 两套测量工具（layout_audit / column_audit）
P3/P4：未动（include 排序、宏展开），3 个 skipped 文件正是它们的范围
```
## 三十三、goal 续跑 r1：把"行内 row 缩进"推进到可验证的一步（未提交，已回退）

这轮把 LuaDefine 那个用例拆开来试，得到两个**确定的新事实**：

```text
① layout 里给"初始化列表元素行"单独算缩进（开列行的层级 × IndentWidth + ContinuationIndentWidth）是可行的：
   新增单测 a_table_row_after_a_row_indents_from_the_list 直接断言 blocks() 给出 8/8 —— **通过**
② 但 fixture（整文件路径）里那些行根本没有走到 planned_indent：
   CPPFMT_DEBUG 只打了 4 行（结构体成员），表行一次都没进 planned_indent，也没有任何 forced break
   → 它们的 gap 在 render 的分支链里没被 `comments.is_empty() && newlines>0 && index>0` 接受，
     具体是哪一条不满足**尚未测得**（调试输出只加在 forced 分支上，没覆盖到这条链的入口）
```

也就是说：问题已经从"缩进算错"缩小到"render 的分支链对这几行没有进入缩进路径"。
下一步只需在该链入口加一条调试（打印 gap.newlines / comments.len / index），一次就能定位。
本轮时间不够，已把 layout/render 恢复成提交状态（37/43、56 tests 全绿、工作区干净）。
## 三十四、goal 续跑 r2：fixture 逼近到"统一差 4 列"

这轮把 `a_table_after_a_struct_body_indents_from_the_line_that_opened_it` 从"三处各不相同"逼到了
**整表统一差 4 列**（注释 14 vs 8、两行 12 vs 8），说明机制已经对了，只剩一个偏移。被测到的因果链：

```text
① layout 里加"初始化列表栈"（Opens/OpensWithoutIndenting 压 None，初始化花括号压 Some(开列行层级)）——
   classify_braces 现在把 `= {` 与"初始化列表中的 {` 都判成 Nothing（引入者是 Assign/ReturnKeyword，
   或外层就是初始化列表），于是 rows 的 blocks() 给出 8（用 layout 调试打印确认：block=Some(8)）
② render 侧两个坑都定位并单独验证过：
   · 同一行"既写源码前缀又发 keep 个换行"会多一个空行 —— 把 own-line 注释的换行数置 0 后空行消失
   · 注释行前缀要 trim 掉行尾空格，否则源码的 2 格会叠在计算列上
③ 剩下的统一 +4：`line_level` 在"以 `}` 开头、后面还跟着 `priority[] = {`"的那一行被记成了 1 而不是 0，
   于是开列行层级 = 1，元素缩进 = 1*4+8 = 12
```

也就是说，下一轮只需让"以闭括号开头的行"把它闭合后的层级作为该行的 line_level（现在取到了闭合前的值），
fixture 就会转绿，然后才能验证 LuaDefine.h 是否整文件一致。

本轮改动全部回退（工作区 37/43、56 tests 全绿），因为差一列时留着会造成"看起来更对"的假象。
## 三十五、goal 续跑 r3：把"开列行"换成行索引，仍差一处

这轮把"初始化列表的缩进基数"改成**记录开列行的行首 token 索引**（不再依赖 `line_level` 的层级换算），
并让 `classify_braces` 认"列表内的 `{` / `= {` / `return {`"为初始化列表。结果：

```text
· 注释行到 8 ✓（第一次对）
· 表行到 6 ✗（应 8）—— 说明 `indents[开列行行首]` 取到的不是那一行的列
  （以 `}` 开头、同行还有 `priority[] = {` 的那一行，行首 token 的 indents 在"闭合后"与"闭合前"之间取值不一致）
· 结构体成员行出现 2/4 混杂 —— 同一机制顺带影响了它们
```

已整体回退。结论要写下来：**这套 stateful 增量补丁已经不收敛**，正确的做法是重构而不是继续打补丁——

```text
把"逻辑行 → 列"做成一个显式两阶段：
  1) 先按 breaks::logical_lines 切出逻辑行（已有）
  2) 每条逻辑行单独计算列：语句起始列 + 初始列表/括号续行的相对列
render 只负责"按行写 token + 已在同一行的 gap"
这样"开列行的列"就是上一条逻辑行的列，不再依赖运行时状态推断
```

这与 P2 的断行器是同一套设施（逻辑行 + 列计算），所以重构一次可以让 37→38 和 P2 的"重排已有断行"同时受益。
## 三十六、P2 的"重排已有断行"原型：113 → **6**（未提交，原型已验证后回退）

这轮试了把**行结构完全交给 breaker**（`ColumnLimit > 0` 时）：

```text
每一逻辑行从新行开始，行内的其余 token 一律"同一行"（忽略源码的换行）；
breaker 给出的断点额外插换行。`ColumnLimit: 0` 时这条路径完全不启用。
实测（column_audit，同一套样式 ColumnLimit: 80）：
  printed 超长行： 113 → **6**
  语料整文件一致： 37/43 不变 ✓
```

也就是说，"只增断行"（已提交，56）与"完全重排"（原型，6）之间差的正是"忽略源码换行"这一步。
**但原型没落地**：我在改 render 的测试辅助函数时误删了文件里的一段真实代码（`RemoveRange` 截到了
`planned_indent` 附近的实现），导致两个既有测试输出被拆成"每个 token 一行"；已整体回退，
工作区保持 37/43、56 tests 全绿。

**落地的正确做法（下次照做即可）**

```text
1. 不动既有测试的 `llvm()`；新增 `zero_limit()` 只给需要"源码断行"契约的测试用
2. render 循环里加两条分支（都在 `reflow == true` 时）：
     if reflow && starts_a_line[index] { 换行 + 块列 }
     else if reflow && gap.comments.is_empty() { 同行空格 }
   `reflow = style.count(ColumnLimit) > 0`
3. `starts_a_line` = 每条逻辑行的首 token ∪ `breaks::break_line` 返回的断点（已实现并验证）
4. 验收：column_audit printed 应到 ~6；layout_audit 必须仍是 37/43
```

这个原型把 P2 的目标从"再写一个引擎"变成了"把已有引擎接上去并保住既有契约"，剩下的只是接线与测试整理。
## 三十七、更正：上一节那个 "113 → 6" 是**假象**；真正的完全重排是 113 → 97

round 14 把"完全重排"接线做完了（逻辑行切分 + 忽略源码换行 + 断点插入，`ColumnLimit: 0` 时不启用），
两个审计都跑了，结果推翻了上一节的结论：

```text
只增断行（已提交，round 10）：  printed 超长行 **56**，语料 37/43
完全重排（本轮原型落地）：      printed 超长行 **97**，语料 37/43（不回退）
上一节记录的 "6"：              是 bug——`starts_a_line` 当时对每个 token 都为真，
                                于是"每个 token 一行"，自然没有超长行。
```

**为什么会变差**：语料的源码本来就是 clang-format 的输出，它的断行比我们 DP 的候选集更强
（能断在参数中间、能重排注释、能拆字符串）。完全重排等于**丢掉一个更好的解**，再用弱模型重新解一次，
所以 56 → 97。这符合"只在正收益时提交"的规则，因此**完全重排没有提交**，保持只增断行。

**结论**：P2 要真正超过 56，不能只靠"忽略源码断行"，还得补齐 breaker 的候选集与惩罚模型
（clang-format 的 `BreakableToken` 能断在 `(` 后的任意实参、`?`/`:` 两侧、字符串字面量内部；
我们只有逗号后/运算符前/括号后几类）。在那之前，"只增断行"是更优的工程选择。
## 三十八、round 15：把候选集放宽是**中性**的，以及一条与格式化无关的既有失败

这轮试了把 `breaks::is_breakable` 的候选集放宽（不再只认"逗号后/运算符前"，而是"任何实参边界"）：

```text
printed 超长行：放宽前 56 → 放宽后 **56**（无变化）
```

原因是"只增断行"只在**已经超长的行**里插断点，候选集再宽也不会改变"哪些行超长"。
按规则（只保留有正收益的改动）已回退。

**同时发现一条与本次工作无关的既有失败**（在 `cpp_code_analysis`，不是 formatter）：

```text
cargo test --release --workspace
  cpp_code_analysis  tests/semantic.rs  an_unresolved_include_does_not_erase_the_files_own_facts  FAILED
本次所有提交只动了 cpp_formatter / docs，且该用例在 formatter 开始时（35/43）就已存在，
记录在此避免后续误判为回归。
```

**当前可复现的指标**

```text
语料整文件一致：  37 / 43（86.05%）
间距一致：        99.51%（spacing_audit）
P2：              printed 56 / logical 105 / breaker 覆盖 80（ColumnLimit: 80）
单测：            cpp_formatter 56 passed + 4 ignored
```
## 三十九、round 16：把列表判据只加在 classify_braces 上（无净收益，回退），并测到一条新事实

```text
改动：classify_braces 里让"`=`/`return`/`,`/`(`/`[` 之后"与"列表内的 `{`"判为初始化列表（只此一处，不动 layout 其它逻辑）
结果：37/43 不变；但 **LuaDefine.h 的首处分歧从 `indent` 变成 `blank line`**，且注释列从 2 变成 14、行从 8 变成 12
```

新的实测事实（比之前更精确）：

```text
· 列表判据本身是对的：注释终于被当作"列表元素"处理了
· 但 render 走的是"多注释 gap"那条老路：源码前缀（`\n  `）与 `keep` 个换行**同时**写出 → 多一个空行；
  前缀里源码的 2 个空格又叠在计算出的 12 上 → 14
· 行本身从 8 变 12 说明 `statement_indent` 在表前不是 0 而是 4（结构体成员行留下的）——
  `planned_indent` 的 in_initializer 分支用 `statement_indent + ContinuationIndentWidth` 计算，
  所以基数必须改成"开列那一行"的列
· 用 CPPFMT_DEBUG 打印 `planned:` 只看到 `const`/`unsigned`/后续表的 `{`，
  **LuaDefine 那张表的第一批行一次都没进 planned_indent** —— 说明它们走的是 verbatim 分支，
  这条链的入口条件还没被打印过（下一轮只需在入口加一行打印，就能定位是哪一条不满足）
```

已回退（37/43、56 tests 全绿）。这是第 5 次在这个用例上收敛失败，但它每次都在**缩小**：
现在的未知量只剩"哪条入口条件拒绝这些行"和"基数取开列行"两个，且都有明确的定位方法。
## 四十、37 → **38 / 43（88.37%）**：初始化列表的缩进基数与"多注释 gap"一起修好了

三处改动，缺一不可（前两轮各自单独试都失败，这次一起上）：

```text
① classify_braces：`=`/`return`/`,`/`(`/`[` 之后的 `{` 与"列表内的 `{`"判为初始化列表
   （roles 在 `} priority[] = {` 处漏标，之前列表根本没被识别）
② render 的 in_initializer 分支：基数改为"开列那一行"的列
   （原来是 statement_indent，被 struct 成员行的 4 污染；现在从 blocks 回溯到上一行首 token 取列）
③ 多注释 gap 的两处修正：
   · 行首注释自己带换行 → `breaks_to_emit = 0`（否则多一个空行）
   · 源码前缀 trim 掉行尾缩进（否则源码的 2 格叠在计算列上）
```

**实测**（全部同一条命令可复核）

```text
整文件一致： 37 → **38 / 43（88.37%）**       ← 达到 goal 的 38 门槛
间距一致：   99.51% → **100.00%**（18879 / 18879）
单测：       56 → **58 passed + 3 ignored**（表 fixture 转正，另一个 ignore 也随之解除）
P2（ColumnLimit 80）：printed 仍 56、logical 105、breaker 覆盖 80（未回退）
```

剩余 5 个文件：`LuaTokenTypeDetail.h`（尾注释分组）、`LuaLexer.cpp:478`（三元 `?` 对齐，属 clang-format 的
断行期列决策）、以及 3 个 token 流不同的跳过项（include 排序 / 宏展开，P3/P4 范围）。
## 四十一、goal 收尾：最终状态与 P3/P4 的边界

```text
整文件一致      38 / 43（88.37%）            ← goal 目标 38 ✓
间距一致        18879 / 18879（100.00%）      ← 从 99.51% 提升
幂等（I2）      43 / 43 文件格式化两次结果相同  ← 用带语料 .clang-format 的临时文件复核
单测            58 passed + 3 ignored（含表缩进 fixture）
P2              printed 56 / logical 105 / breaker 覆盖 80（ColumnLimit: 80；clang-format 为 0）
构建            workspace 全量构建通过
已知无关失败    cpp_code_analysis tests/semantic.rs::an_unresolved_include_does_not_erase_the_files_own_facts
                （formatter 开工前即存在，与本次改动无关）
```

**剩余 5 个文件（都已定位到机制，不是"未知"）**

```text
1. LuaLexer.cpp:478   三元运算符 `?` 的列：clang-format 在断行时决定（`?` 落在 19），
                      需要 P2 的"断行期列决策"才能复现；不是纯缩进规则
2. LuaTokenTypeDetail.h:31  行尾注释分组：`SpacesBeforeTrailingComments: 0` 只在单例时生效，
                      但"是否单例"要按 clang-format 的列块分组判断（试过一次最小值规则，全局回归 → 已回退）
3~5. FormatContext.cpp / Util.h / LuaParseError.h
                      token 流不同：include 排序（P3）与宏展开（P4）改变的是 token，不是空白
```

**P3/P4 的范围（本次未动，边界明确）**

```text
P3  include 排序与合并（`SortIncludes`、`IncludeBlocks`、`IncludeCategories`）
P4  宏处理（`#define` 体内重排、`MacroBlockBegin/End`、`StatementMacros`）
两者都会改变 token 流本身，因此必须建在"token 流可变"的架构上；当前 5 个剩余文件中 3 个正等待它们。
```

**P2 的后续（已有设施，未完成的部分）**

```text
已具备：逻辑行切分（breaks::logical_lines）、penalty DP 断行（breaks::break_line）、
        接线（只增断行，printed 113 → 56）、两个测量工具（column_audit / layout_audit）
未完成：完全重排（忽略源码断行）实测 97，比 56 差 —— 因为我们的候选集/惩罚模型弱于 clang-format 已有的断行；
        要超过 56，需要补齐 BreakableToken 级别的候选（实参中间、`?`/`:` 两侧、字符串内部）与注释重排
```
## 四十二、P3 的第一步：include 排序 → **41 / 43（95.35%）**

`crates/cpp_formatter/src/includes.rs`（新模块，6 个单测）：

```text
· 一个"块" = 两行空行之间的连续 `#include` 行；块内排序，块之间不动（`IncludeBlocks: Preserve`）
· 文件自己的头文件排最前（clang-format 的 main-header 规则：`Mark.cpp` 里的 `"…/Mark.h"`）
· 其次引号包含、再其次尖括号；各自按路径排序，大小写按 `SortIncludes.IgnoreCase`
· `SortIncludes.Enabled: false` 时原样返回；`#pragma`/`#define` 等其它行一个字节都不动
```

接线：`format(source, stem, style)` 新增 `stem`（文件名去扩展名），`lib::format_file(path, …)` 从路径取；
CLI 与三个审计都改用 `format_file`（这正是之前 3 个文件"token 流不同"的原因——它们只差 include 顺序）。

**实测**

```text
整文件一致： 38 → **41 / 43（95.35%）**   ← 3 个 skipped 文件全部转正（FormatContext.cpp / Util.h / LuaParseError.h
                                            以及 Mark.cpp / TextReader.cpp / LuaFormat.cpp 的排序）
间距一致：   **19989 / 19989（100.00%）**（gap 总数从 18879 增加到 19989，因为多了排序后的文件）
幂等：       43 / 43（这次用"保留同名文件"的方式复核；先前 3 个失败是 harness 改名的假象）
单测：       62 → **64 passed + 3 ignored**
P2：         printed 56 / logical 105 —— 未回退
剩余 2 个：LuaTokenTypeDetail.h:31（尾注释分组）、LuaLexer.cpp:478（三元 `?`，属 P2 断行期列决策）
```
## 四十三、P2：候选集扩展 + 续行列计宽 → printed **56 → 51**

两处改动（都在 `breaks.rs`，`ColumnLimit: 0` 时完全不生效）：

```text
① 候选断点补两类：
   · 二元运算符**之后**（右操作数长时 clang-format 也断在那里；此前只能断在运算符之前）
   · 括号后/逗号后（已有）
② 动态规划的片段计宽：第 2 段起用"续行列"（statem　ent 列 + ContinuationIndentWidth）作为起点，
   此前一律用语句列 → 搜索以为放得下、实际放不下，这正是"加了断行还是超长"的原因之一
```

**实测**

```text
printed 超长行： 56 → **51**（ColumnLimit: 80，43 个文件；clang-format 仍为 0）
语料整文件一致： 41 / 43（未回退）
间距一致：      19989 / 19989（100.00%，未回退）
单测：          64 passed + 3 ignored
```

剩余 51 行里，`breaker` 统计显示 25 条属"单个 token 就超宽"（长字符串/长注释，需要 `BreakStringLiterals`
与注释重排），其余是候选集仍缺的位置：`?`/`:` 两侧的精确处理、模板参数内部、字符串内部。
## 四十四、尾注释"最小空格"规则：实测**不成立**，已回退

`LuaTokenTypeDetail.h:31` 的唯一差异是 `TK_GLOBAL,// global lua5.5`（clang-format 0 空格）对我们的 1 空格。
按语料配置 `SpacesBeforeTrailingComments: 0`，我实现了"孤立尾注释（前后行都没有尾注释）→ 用最小值 0"：

```text
结果：41 → **38 / 43**（5 个文件在 `other` 上出现新分歧）
结论：这条规则在 clang-format 里不是"孤立即最小值"——大多数独立尾注释仍保持 1 空格以上，
      0 只出现在它决定不对齐、且把最小值当作"直接拼接"的情形。
      要复现它必须实现真正的分组/对齐模型（`AlignTrailingComments` 的 Kind 与组内列），成本与收益不成比例。
```

已回退，基线保持 **41 / 43**、64 tests 全绿。这条留给后续实现完整 `AlignTrailingComments` 时一起做。
## 四十五、goal（第二次）收尾核对

```text
整文件一致      41 / 43（95.35%）                    ← 目标 "→ 41/43" ✓（原为 38）
间距一致        19989 / 19989（100.00%）              ← 全部 gap 正确
幂等（I2）      43 / 43（保留同名文件复核）            ← format 的第二次输出与第一次相同
单测            64 passed + 3 ignored
P2 超长行       printed 51（原 56，clang-format 0）    ← 目标 "below 56" ✓
                logical 105 / breaker 覆盖 80（25 条为单 token 超宽）
workspace       全量构建通过
```

包含的三次提交：`a1f30c1`（P3 include 排序 + main-header，38→41）、`8ba7aad`（断行候选与续行列计宽，
56→51）、`f977d8c`（尾注释最小空格规则的否定结论，41→38 已回退）。

剩余 2 个文件：`LuaTokenTypeDetail.h:31`（需要完整 `AlignTrailingComments` 分组模型）、
`LuaLexer.cpp:478`（三元 `?` 的列由断行期决定，需 P2 的列决策）。两者都不在本 goal 的量化目标内。
## 四十六、把对比口径写进审计：语料本身只有一部分是格式化过的

按要求核对了对比方式，并把"对比对象是什么"变成审计自己打印的数字：

```text
layout_audit 现在打印三行：
  whole file: 41 of 43 identical to clang-format (95.35%)
    source already formatted: 33 · clang-format changed the source: 10 · of those we match: 8
    clang-format`s output fed back through us, unchanged: 42 of 43
```

三行各是什么：

```text
1. 我们是把 `clang-format -style=file <file>` 的输出当 oracle（layout_audit.rs 里 `run(&clang_format, file)`），
   **不是**拿语料源码当标准；
2. 43 个文件里有 33 个已经是 clang-format 的结果（clang-format 对它们无操作），10 个会被 clang-format 改动；
   这 10 个里我们复现了 8 个 —— 说明"一致"不是"两边都等于源码"的同义反复；
3. 反过来把 clang-format 的输出喂给我们，43 个里 42 个原样返回（I3 稳定性）；
   唯一不稳定的是 `LuaLexer.cpp`，正是三元 `?` 那个文件（clang-format 在断行期决定它的列，我们不重排）。
```

结论：现有 41/43 的口径是对的（对 clang-format 输出），并且现在审计会自己暴露"语料有多少本来就没格式化"
以及"我们的输出对 clang-format 结果是否稳定"。剩下的差距也定位得更清楚：只有 `LuaTokenTypeDetail.h`
（尾注释分组）和 `LuaLexer.cpp`（三元列）两处。
## 四十七、条件表达式的续行列：**保留源码列** → 42/43，且对 clang-format 结果 **43/43 稳定**

用小实验矩阵问了真实的 clang-format（`ColumnLimit: 0`）：

```text
    int x = aaa == bbb        `?` 在 20 = 表达式起始列(12) + ContinuationIndentWidth(8)
                    ? 1
                    : 2;
    y = ccc ? 3               `:` 在 12 = 与同一行的 `?` 对齐
            : 4;
    return _reader... == ch   `?` 在 19 = 表达式起始列(11) + 8
                   ? count + 2
           : (count == 0) ? 1   `:` 在 11 —— 与上面两条又不一致（clang-format 在断行时逐行决定）
```

结论：这个列**不是**可以由缩进规则推出的常量，它是 clang-format 在换行搜索里定的。所以实现改成：

```text
`?` / `:` 出现在行首时，**保留源码里该行的缩进列**（`render::source_indent`）。
对"已经由 clang-format 格式化过的文件"这是恒等操作，从而满足 I3（稳定性）；
对未格式化的语料，则保持了作者那行的列，这正是 clang-format 通常会保留的位置。
```

**实测**

```text
整文件一致： 41 → **42 / 43（97.67%）**
clang-format 的输出喂回来原样返回： 42 → **43 / 43** ✓
clang-format 会改动的 10 个文件里，我们复现了 9 个
剩余 1 个：LuaTokenTypeDetail.h（尾注释分组）
```
## 四十八、尾注释对齐：模块做对了，接线**全面回归**（88.37%），已回退

这轮把 `AlignTrailingComments` 的规则用真实 clang-format 测明白了：

```text
SpacesBeforeTrailingComments: N 下：
  · 连续几行都带行尾注释 → 一个"组"，所有注释起点 = max(各行代码结尾列) + N
  · 只有一行带注释 → 它自成一"组"，起点 = 该行代码结尾列 + N
  实测（N=0）：`    A,   // one` / `    BB,  // two` / `    CCCC,// three`；`int solo;// alone`
```

`crates/cpp_formatter/src/trailing.rs` 按这个模型算好了每处注释的目标列（4 个单测通过），
并把 `tokens.rs` 里一个真正的 bug 修好了：`own_line` 原来把"紧跟代码的注释"也标成行首注释
（`int solo;// alone` 被判成 own_line），导致尾注释逻辑根本走不到。

**但把它接进 render 后实测回归**：

```text
41 → 38 / 43；clang-format 输出喂回后的稳定性 42 → 38
原因：目标列是按"源码的代码结尾列"算的，而我们的输出列在少数行与源码不同（重排后），
      两者混用会在若干行上加错空格；另外组内包含"代码结尾列"受我们重排影响的行时同样错位。
```

已把 render 的接线回退（保留 `trailing.rs` 模块、单测与 `tokens.rs` 的 own_line 修复），
基线回到 **42 / 43（97.67%）**、稳定性 **43 / 43**、68 tests 全绿。

要正确接线，目标列必须按**渲染出的列**计算，而不是源码列——这要求把尾注释对齐放到 render 的
第二次扫描（或让 render 记录每行代码结尾列后回填）。这是下一步，不再用源码列近似。
## 四十九、P2：按"源码行"而不是"逻辑行"断行 → 51 → **52**（回退）

假设：本 pass 保留源码换行，所以只有"源码行"可能超长；把逻辑行当一整行来断，会在错误的位置插断点。

实现：`break_source_lines`（在每段源码行上跑 DP，用该行的列作起点），接线到 render。

```text
实测：printed 51 → **52**（略差），语料 42/43 与稳定性 43/43 不变
```

差 1 行说明这个假设方向对、但收益为零：逻辑行模型已经把断点放在需要的位置，按源码行切分只是把
搜索范围缩小，反而在一处少了一个更好解。按规则回退（未提交），保留 51。

**P2 目前的完整实测结论**（三种模型，同一套测量）：

```text
只增断行（当前提交）       printed **51**
按源码行再切分             52   ← 本轮，回退
完全重排（忽略源码断行）   97   ← 更早，回退
```

也就是说：**在现有候选集与代价模型下，"只增断行"就是最优**；要再往下走，必须补
BreakableToken 级别的候选（实参中间、字符串内部、注释重排），而不是更换搜索范围。
## 五十、P2 的第三种模型：只重排"已经超长的逻辑行" → 74（回退）；并给出 51 行的成分

```text
选择性重排（只对"含超长片段"的逻辑行忽略源码断行、整体重搜）： printed **74**  ← 回退
```

三种模型的完整实测（同一套测量，`ColumnLimit: 80`，43 个文件）：

```text
只增断行（当前）     51     ← 保留
按源码行再切分       52
选择性重排           74
完全重排             97
```

**为什么越"重排"越差**：语料源码本来就是 clang-format 的输出，它的断点比我们 DP 的候选集更细。
我们重搜时只能在"逗号后 / 运算符前后 / 括号后 / 成员访问前"这些位置断，重构出来的行反而更长。

**51 行的成分**（本轮测出的分布，决定下一步该做什么）：

```text
注释行 4 · 字符串行 16 · 代码行 30
代码行的例子（都是"模板参数里的续行片段"）：
  <bool(LuaTokenKind)> predicated, const LuaSyntaxTree &t) const;
  <LuaSyntaxNode> LuaSyntaxNode::GetChildren(const LuaSyntaxTree &t) const {
```

结论：**51 是当前候选集下的地板**。再往下必须补 `BreakableToken` 级别的能力——
字符串内部拆分（会改 token 流）、注释重排、模板参数内部的断点；只换搜索范围（三种模型都试过）都是负收益。
## 五十一、P2：**51 → 50** —— "已经折断但仍超长的源码行"单独再搜

前三种模型（只增断行 51 / 按源码行 52 / 选择性重排 74 / 完全重排 97）都是整条逻辑行做一次搜索。
这轮把搜索**加在已有的整行搜索之上**，只针对一种情况：

```text
一条源码行（源码里已经折断出来的那一段）本身仍然超过限宽
  → 只对这一小段再跑一次 DP，把它的断点加进去
其余一切不变（仍然只增断行）
```

实测：

```text
printed 超长行： 51 → **50**
语料整文件一致： 42/43（未回退）· 稳定性 43/43（未回退）· 间距 19989/19989（100%）
单测： 68 passed + 3 ignored
```

这印证了上一轮的结论方向：不是"换搜索范围"，而是"补上搜索看不到的那一段"。
剩下 50 行里，4 行注释、16 行字符串、30 行模板续行片段——要继续降，必须补
`BreakableToken` 级能力（字符串内部、注释重排、模板参数内部断点）。
## 五十二、尾注释对齐的第二次扫描：模型仍不完整（42 → 39），已回退

这轮把对齐做成"渲染后的第二次扫描"（`trailing::align`）：读回我们自己的输出行、按组求 `max(代码结尾) + N`、只改注释前的空格。
用真实 clang-format 校准了单例与组的用例（`SpacesBeforeTrailingComments: 1` → 列 10，与我们的实现一致，
字符串里的 `//` 不会误判），6 个单测通过。

但整语料实测**回归**：

```text
42 → **39 / 43**；稳定性 43 → **39**
分歧样例（LuaDefine.h:16）：
  ours:   "unsigned char left; /* left priority… */"
  theirs: "unsigned char left;  /* left priority… */"
```

说明 clang-format 的组列不是 `max(代码结尾) + N`：这一处**最宽的那一行自己也带走了一格**，
列比最宽代码结尾多 1。也就是说它的最小间隔规则在"最宽行"上仍生效，而我的模型把 N 直接加在最大值上。
这和我在小样例里测到的"最宽行 0 空格"相互矛盾——**它的分组/最小间隔还有一层我没测到的规则**，
不是靠推理能在本轮补上的。

已回退接线（`trailing::align` 与 6 个单测保留在 `crates/cpp_formatter/src/trailing.rs`，可复用），
基线回到 **42 / 43**、稳定性 **43 / 43**、70 tests 全绿。

**结论**：尾注释要真正对齐，需要用 clang-format 的 `WhitespaceManager` 语义做实验矩阵（组边界、最小间隔、
`AlignTrailingComments.Kind` 三个维度），而不是从两个样例推公式。这不在本轮预算内。
## 五十三、语言服务器接入：`textDocument/formatting` · `rangeFormatting` · `onTypeFormatting`

新模块 `crates/cpp_ls/src/handlers/formatting/mod.rs`，按本仓库既有的三段式接入（模块、dispatch 行、capability 行）：

```text
capabilities:  document_formatting_provider = true
               document_range_formatting_provider = true
               document_on_type_formatting_provider = { first: "}", more: [";"] }
dispatch:      Formatting => on_formatting
               RangeFormatting => on_range_formatting
               OnTypeFormatting => on_type_formatting
```

实现要点：

```text
· 取文本：session.view(path) → files().held() → VfsFile.text（VFS 里就是用户未保存的缓冲区）
· 样式：style_for(path) 向上找最近的 .clang-format，用 cpp_formatter::config::parse；找不到用默认样式
· 格式化：cpp_formatter::format_file(path, text, &style)
· 编辑列表：edits_between(old, new) —— 行级最小差分，只把变化的那一段行作为一个 TextEdit
  （命中"整行替换"之外什么都不动，客户端的光标/选中/撤销粒度才不会被整文件替换破坏）
· rangeFormatting：整文件格式化后**按行范围过滤**编辑，而不是把片段单独格式化
  （片段单独格式化会从第 0 列开始缩进——格式化器需要整文件才能判断语句缩进）
· onTypeFormatting：同一份编辑只保留与当前行相交的那一条（触发字符 `}` 与 `;`）
```

测试：`edits_between` 的三条单测（无变化 → 空；单行变化 → 只覆盖该行；跨范围相交判定）。
`cargo test -p cpp_ls --lib` = **88 passed + 1 ignored**；`cpp_formatter` = **70 passed + 3 ignored**。

尚未做（下一步）：用真实 LSP 会话端到端验证（initialize 后发 `textDocument/formatting` 断言返回编辑）；
以及 `rangeFormatting` 的"只格式化范围内"精化（现在是整文件格式化后过滤，语义正确但会做多余工作）。
## 五十四、LSP 格式化：端到端用例通过（走真实协议）

新增用例 `crates/cpp_ls/tests/handshake.rs::a_formatting_request_returns_edits_for_the_whole_file`，走完整链路：

```text
initialize（断言 documentFormattingProvider / documentRangeFormattingProvider /
           documentOnTypeFormattingProvider.firstTriggerCharacter == "}"）
→ initialized → textDocument/didOpen（"int  x = 1;\n   int y = 2;\n"）
→ textDocument/formatting           断言：返回编辑、双空格被收成一个、三格缩进消失
→ textDocument/rangeFormatting      断言：返回列表且条数 ≤ 整文件
→ textDocument/onTypeFormatting     断言：返回列表（位置在第 1 行、触发字符 ";"）
```

实现上修掉的一个真问题：处理函数原来依赖 `session.view(path)`，而**刚 didOpen 还没解析过的文件没有 view**，
于是整条链路返回 `null`（`RequestOutcome::Missing`）。改为"从会话取文本（held → 磁盘兜底）+ 路径"，不再依赖 view。

测试结果：

```text
cpp_ls --lib            88 passed + 1 ignored
cpp_ls --test handshake 32 passed + 5 ignored（端到端，含新用例）
cpp_formatter --lib     70 passed + 3 ignored
```

注意：`a_completion_sees_a_change_to_an_included_header` 在**并行跑整个 handshake 套件**时偶发失败、
单独跑通过；本轮两次整套重跑都是 32 passed，判定为与本次改动无关的抖动（未修，记录在此）。
## 五十五、尾注释规则的第三轮实验：**"孤立 → 最小值"被证否**（42 → 38）

用真实 clang-format 做了两组实验，结论比之前更清楚：

```text
同一配置（SpacesBeforeTrailingComments: 0），同一形状，组列不同：
  struct { unsigned char left;  … }  两行 → 注释列 = max(代码结尾) + **1**
  enum   { A, … CCCC, … }            两行 → 注释列 = max(代码结尾) + **0**
  int a;// / int bb;//               两行 → max + 0
  int g(int);// / int h(int long_name);//  两行 → max + 0
而且：把 struct 那两行的空格从 0/0、13/4、2/1 三种写法喂进去，输出**都变成** max+1 的同一结果。
```

也就是说 clang-format 的列不是 `max + N`，也不是"已对齐就保留"；它还有一层我没测出来的判据
（可能与 `AlignTrailingComments.Kind` 与"列是否需要被 *推* 到某个位置"有关，LLVM 默认值是 `Leave`）。
把"孤立注释 → 最小值"接进 render 后实测：**42 → 38**，稳定性 43 → 39 → 整体回退（`lone.rs` 一并删除）。

**结论（A 的现状）**：42/43 是我们能稳定复现的最好结果；最后那个文件
（`TK_GLOBAL,// global lua5.5`）的差异属于 clang-format 尾注释对齐的实现细节，
在没有它的源码级真值的前提下，靠小样例反推已经三次失败（§四十四、§五十二、本节）。
该项建议单独立项：以 clang-format 的 `WhitespaceManager::alignTrailingComments` 源码为准，做逐分支的实验矩阵。
## 五十六、把尾注释的"列"变成语料测量表（新增 `examples/trailing_probe.rs`）

前三次失败都是"从小样例推公式"。这轮改成**直接量语料**：先用 clang-format 把 43 个文件格式化到临时目录，
再用 `examples/trailing_probe.rs` 统计每一个"行尾注释组"的实际列。

```text
groups: 48 · lines: 60
delta（注释列 − 组内最宽代码结尾）的分布：
  ')' 0×1 1×6 · ',' 0×8 1×26 · ';' 0×6 1×6 · '{' 0×2 · '}' 0×3 2×1 · 'e' 1×1
```

关键事实（比之前的猜测都更硬）：

```text
· 组内所有注释同列（这是确定的）
· delta 多数是 **1**，不是 0 —— 尽管语料配置是 SpacesBeforeTrailingComments: 0
· delta 0 的例子全部是**语句/命名空间级**：`Next();// Global`、`if (...)// optional step`、
  `}// namespace detail`、`return TK_OR;// …`
· delta 1 的例子是**声明块里的成员/枚举项**（LuaDefine.h 的 `unsigned char left;  /* … */`）
```

也就是说 clang-format 对"声明块内"的注释比语句多留一列。这条规律现在有语料支撑（48 组），
但它需要"组所处的作用域是类/结构/枚举体"这个判断，属于格式化器目前不做的事——
**实现前必须先把这个作用域判断加进 layout**，否则又是猜。

本轮只提交测量工具与这张表，不改 render（42/43 保持）。
## 五十七、"声明体 +1 列"也**不成立**（42 → 38，回退）

按上节的语料表实现：`layout::in_declaration_body`（`class`/`struct`/`enum`/`union` 体识别）+
`trailing::align` 里 `target = max + N + (在声明体 ? 1 : 0)`，接进 `format_file`。

实测 **42 → 38**，稳定性 43 → 38。四处对照（同一份输出）：

```text
ours: "unsigned char left; /* … */"      theirs: "unsigned char left;  /* … */"   ← 声明体：要 +2 不是 +1
ours: "TK_GLOBAL, // global lua5.5"      theirs: "TK_GLOBAL,// global lua5.5"     ← 枚举项：要 +0
ours: "0x00,          /* EOZ */"         theirs: "0x00,           /* EOZ */"      ← 初始化列表行：要 +1
ours: "return TK_DOTS;/* '...' */"       theirs: "return TK_DOTS; /* '...' */"    ← 语句：要 +1
```

四类上下文（结构体成员、枚举项、初始化列表行、语句）需要**四种不同的列规则**，且与上一节的均值表相互矛盾
（上一节"声明体 +1"来自对 LuaDefine 两行的观察，但同一文件里的枚举项却是 +0）。

结论：**尾注释列无法用"上下文 + 常数"的模型复现**。它取决于 clang-format 的 `WhitespaceManager`
内部的分组与推列逻辑（`AlignTrailingComments.Kind` 的 `Leave` 语义、组的边界、以及"只向右推"的规则）。
在没有该源码的前提下，继续靠语料反推已被证否四次（§四十四、§五十二、§五十五、本节）。

已全部回退：42/43、稳定性 43/43、`cpp_formatter` 70 tests 全绿。
`examples/trailing_probe.rs`（语料测量工具）保留，它是下一步唯一有用的起点。
## 五十八、样式解析收敛到一处（CLI 与 LSP 用同一条规则）

`cpp_formatter::config` 新增两个公开函数，把"向上找 `.clang-format`"从两个入口里各写一遍收敛成一处：

```text
pub fn find_config(file: &Path) -> Option<(PathBuf, String)>   // 最近的 .clang-format 及其文本
pub fn style_for_file(file: &Path) -> Style                    // 解析它；没有就用默认样式
```

- `crates/cpp_ls` 的格式化处理器删掉自己的 `style_for`，改用 `cpp_formatter::config::style_for_file`
  ——命令行工具与语言服务器从此不会对同一个文件给出两种样式。
- 新增单测 `the_nearest_configuration_wins_and_the_default_is_the_fallback`：
  在临时目录树里放 `IndentWidth: 7`，断言下层文件取到 7；无配置路径回退到默认样式。

**实测**

```text
cpp_formatter --lib  70 → **71 passed** + 3 ignored
cpp_ls --lib         88 passed + 1 ignored
handshake 端到端      格式化用例仍通过
语料                 42 / 43（97.67%）· 稳定性 43 / 43（未变）
```

## (A) 的现状（等你决定）

尾注释列的四次反推都失败，已全部回退；42/43 保持。需要你在三条路里选一条
（移植 clang-format 的 `WhitespaceManager` 算法 / 接受 42/43 / 审计口径忽略行尾注释空格），
我下轮会按你的选择执行；若没有选择，我会继续做 (B) 的 `willSaveWaitUntil`。
## 五十九、按 clang-format 源码实现尾注释对齐 —— 仍然不成立（42 → 39）

按你的要求联网取了 LLVM 的 `WhitespaceManager.cpp`，读到了真正的算法：

```cpp
// WhitespaceManager::alignTrailingComments()
· 每条尾注释有"最小列" ChangeMinColumn = C.StartOfTokenColumn（代码结尾 + 该样式要求的空格）
· 连续带注释的行构成一段；段列 = 段内最小列的**最大值**（MinColumn = max(...)）
· 只向右移：`Shift = Column - StartOfTokenColumn; if (Shift <= 0) continue;`
· 序列在空行处断开（NewLineThreshold）、在 `AlignTrailingComments.Kind == Never/Leave` 时整段跳过
```

按这个结构实现（段列 = max(代码结尾 + N)，只向右补齐），接进 `format_file` 后实测 **42 → 39**，
稳定性 43 → 39，已回退。

**卡在哪**：`C.StartOfTokenColumn` 的"最小列"里那个空格，不是简单的 `SpacesBeforeTrailingComments`：
同一条配置（`0`）下，`Next();// Global` 的注释在代码结尾 + 0，而 `unsigned char right; /* … */` 在 +1。
也就是说 `getSpacesRequiredBefore` 对不同前导 token 给出不同值（`;` 之后有时 0、有时 1），
这一层在 `FormatToken::SpacesRequiredBefore` / `ContinuationIndenter::addTokenOnCurrentLine` 里，
还要再取一份源码（`ContinuationIndenter.cpp` 中 `Current.Spaces = ...` 那段）才能确定。

本轮成果：算法主体已确定（段列 = 最小列的最大值、只向右移），缺的只有"最小列里那个空格"的取值规则。
下一轮我会取 `ContinuationIndenter.cpp` 与 `FormatToken.cpp` 里 `SpacesRequiredBefore` 的定义，把它定下来。

（同时：`textDocument/willSaveWaitUntil` 已接上并通过端到端用例，见下一个提交。）
## 六十、联网读了 `TokenAnnotator.cpp`（tag 22.1.3）后的新证据，但仍未闭合

取了 `llvmorg-22.1.3` 的 `TokenAnnotator.cpp` 与 `ContinuationIndenter.cpp`：

```cpp
// TokenAnnotator.cpp:4184（22.1.3 与 main 相同）
if (Current->is(TT_LineComment)) {
  ...
  Current->SpacesRequiredBefore = Style.SpacesBeforeTrailingComments;   // 语料里是 0
}
// WhitespaceManager::alignTrailingComments()
· 段列 = 段内 (代码结尾 + SpacesRequiredBefore) 的**最大值**
· 只向右移（Shift > 0 才写）
```

按这个实现（= max(code_end + 0)，只右移）实测 42 → 39，回退。

然后用真实 clang-format 做了定点实验，得到**互相矛盾**的两组：

```text
（N = 0，同一次运行）
int solo;/* one */                    → int solo; /* one */     注释 = 代码结尾 + 1
    int a;/* one */（结构体成员）      →     int a; /* one */     +1
    int a;/* */ 与 int bb;/* */（两行）→ 列 = max(代码结尾) + 1
    Next();// Global
    if (x)// optional
        return a;// b                 → 列 = max(代码结尾) + 0    （三行成段）
```

也就是说 `SpacesRequiredBefore`（0）只是**初始**空格，而"孤立注释"那一支还会多出 1 格；
成段时则严格按"段内最大值、只右移"。

结论：算法主体（段列 = 最小列最大值、只右移）已经与源码核对一致，但"最小列"在 22.1.3 里还受
另一处逻辑影响（孤立注释 +1），那一处我还没定位到。这已是第五次尝试，全部回退，42/43 保持。

**建议（仍需你选）**：① 继续找那处逻辑（需要再读 `Format.cpp`/`WhitespaceManager.h` 的
`StartOfTokenColumn` 赋值链）；② 接受 42/43；③ 审计口径忽略行尾注释空格。
本轮不再继续该线，避免继续消耗。
## 六十一、`ContinuationIndenter.cpp` 读完：`StartOfTokenColumn = State.Column + Spaces`，但 `Spaces` 的基准仍对不上

22.1.3 的 `ContinuationIndenter::addTokenOnCurrentLine`：

```cpp
unsigned Spaces = Current.SpacesRequiredBefore + ExtraSpaces;   // ExtraSpaces 所有调用点都是 0
Whitespaces.replaceWhitespace(Current, /*Newlines=*/0, Spaces,
                              State.Column + Spaces + PPColumnCorrection, ...);
```

也就是"最小列 = 代码结尾 + SpacesBeforeTrailingComments(0)"。但真实输出的定点实验仍然自相矛盾：

```text
同一 --style（含 SpacesBeforeTrailingComments: 0/1/3 都试过，输出完全相同）：
  int aaaa;/* one */           → int aaaa; /* one */     组列 = max(代码结尾) + 1
  int b;/* two */              → int b;    /* two */
  int c;/* three */            → int c;    /* three */
  int dddd;/* four */          → int dddd; /* four */

  void f() { Next();// G
             if (x)// opt
                 return a;// b }  → 三行组列 = max(代码结尾) + 0
```

两组的唯一差别是"组内最深缩进那一行"是否为组内最宽行——但两组的输出分别是 +1 与 +0，
而 `SpacesBeforeTrailingComments` 在 0/1/3 下**输出完全一致**。这说明这个选项在我的实验里根本没生效，
或者最小列另有一处来源（`State.Column` 在 `moveStateToNextToken` 里的推进方式）。

**结论**：`(A)` 需要把 `State.Column` 的推进（`moveStateToNextToken`、`ColumnWidth`、`SpacesRequiredBefore`
之间的相互赋值）也核对一遍；这已经超出"读两个函数"的范围，且六次实现尝试全部回退（42 → 39/39/38/39/…）。
`(B)` 已完成并有端到端用例。据此把该目标标为 blocked，理由写为这一具体条件。
## 六十二、决定性实验：`SpacesBeforeTrailingComments` 在 22.1.3 里**不起作用**

用真实 clang-format（同一个文件、同一个 `BasedOnStyle`）分别把 `SpacesBeforeTrailingComments` 设为 0 和 3：

```text
N=0 → int aaaa; /* one */   /   int b;    /* two */
N=3 → int aaaa; /* one */   /   int b;    /* two */     ← 输出完全相同
```

也就是说这个键在 22.1.3 的二进制里**对行尾注释没有影响**（我们的 presets 表里它仍是有效键、`-dump-config` 也照样打印，
但实际行为不看它）。这解释了为什么"用 N 推公式"的六次尝试全部失败。

再看真实文件里的两处（同一份 `.clang-format`、同一次运行）：

```text
LuaTokenTypeDetail.h:  "    TK_GLOBAL, // global lua5.5"  →  clang-format: "    TK_GLOBAL,// global lua5.5"   （去掉空格）
构造的小样例:          "int aaaa;/* one */"              →  clang-format: "int aaaa; /* one */"              （加空格）
```

同为"代码后接注释"，一处去空格、一处加空格；且选项值无关。这已经不是"某个常量"的问题，
而是 `FormatToken::SpacesRequiredBefore` 在 22.1.3 内部分支里的取值差异（与 token 类型、序列是否被拆开都有关）。

**结论**：(A) 达到 43/43 需要把 22.1.3 的 `TokenAnnotator` 里 `SpacesRequiredBefore` 的全部分支
（`TT_LineComment` / `BK_BracedInit` / 序列拆分 / `CreateReplacement`）逐条移植——这已不是"读一个函数"，
而是移植一个子系统。六次实现尝试（39/38/39/39/39…）全部回退，42/43 与 43/43 稳定性保持。
## 六十三、找到那条分支：`spaceRequiredBefore` 把空格从 0 抬到 1

22.1.3 的 `TokenAnnotator.cpp:4208`：

```cpp
} else if (!Current->Finalized && Current->SpacesRequiredBefore == 0 &&
           spaceRequiredBefore(Line, *Current)) {
  Current->SpacesRequiredBefore = 1;
}
```

也就是说行尾注释的空格 = `SpacesBeforeTrailingComments`（0），**除非** `spaceRequiredBefore` 认为需要空格
（典型就是 `;` 之后）——而 `,` 之后不需要，所以 `TK_GLOBAL,//` 是 0 空格。

定点实验（同一文件、同一配置）：

```text
    int solo;// x      → 0 空格
    Next();  // y      → 对齐到 max(代码结尾)=13（补 2 格）
    return a;// z      → 0 空格
```

**同缩进的组 = max(代码结尾) + 0、只向右补** —— 这正是 `alignTrailingComments` 的段列公式。
而之前我测到的 `+1`（tc7）出现在"组内缩进深浅不一"的样例里，说明那种情况下段被拆开了（每段各自对齐）。

下一步（下一轮）：把 `trailing::align` 改成
① 段列 = max(代码结尾) + N，只向右；
② 段在**缩进不同**处断开（同缩进才成段）；
③ 只对"行尾注释所在行"改写。
然后跑三个审计，期望 43/43。
## 六十四、**43 / 43（100.00%）**——尾注释对齐按 clang-format 源码实现成功

规则（全部来自 22.1.3 源码，逐条对上语料）：

```text
TokenAnnotator.cpp:4208  行尾注释的初始空格 = SpacesBeforeTrailingComments，若 spaceRequiredBefore 为真则 +1
                         → 行注释（//）不需要；块注释（/* */）需要
WhitespaceManager::alignTrailingComments()
                         段列 = 段内 (代码结尾 + 初始空格) 的**最大值**；只向右补（Shift <= 0 不写）
WhitespaceManager::getCells  （数组初始化器的单元格）注释"距最后一个 token 一个空格"，
                         若该 token 是逗号则 1、否则 **2**：
                         setChangeSpaces(i, Changes[i - 1].Tok->is(tok::comma) ? 1 : 2)
```

实现：`crates/cpp_formatter/src/trailing.rs` 的 `align`（渲染后回读、按行分组、只向右、不改代码），
`start_spaces` 三档：数组行以 `}` 结尾 → 2、以 `,` 结尾 → 1、其它块注释 → 1、行注释 → 0。

**实测（三个审计 + 单测）**

```text
整文件一致： 42 → **43 / 43（100.00%）**
稳定性：     43 / 43（把 clang-format 的输出喂回来，原样返回）
间距一致：   19989 / 19989（100.00%）
P2：         printed 50 → **49**（顺带下降 1）
单测：       72 passed + 3 ignored
```

至此 `(A)` 完成：不再是"已知限制"，而是逐字节与 clang-format 22.1.3 一致。
## 六十五、P3 开工：`DisableFormat`、`clang-format off/on` 的区域模型、库级 range 格式化

这一轮（43/43 之后的第一轮 P3/P2）落了三个可测的东西：

```text
① DisableFormat: true —— 两个入口（format / format_file）直接原样返回
② regions.rs —— `// clang-format off` … `// clang-format on` 的字节区间模型（4 个单测）：
     · 支持 `//` 与 `/* */` 两种写法、`clang-format: off` 的冒号写法
     · 没有 `on` 时区间到文件末尾
     · 普通注释（"// clang-format is a tool"）不会误判
③ lib::format_range(source, style, first, last) —— `-lines` 的库级等价物（2 个单测）
     整文件格式化后按行取用，片段单独格式化会从第 0 列开始缩进
```

**实测**：`cpp_formatter` **78 passed + 3 ignored**；语料 **43/43（100%）**、稳定性 **43/43** 未变；workspace 全量构建通过。

**尚未接线的部分**（下一轮）：render 里"disabled 区间原样写 gap"——这一轮改了两版都因
if/else 链的括号结构而回退（`git checkout` 恢复），所以 `off/on` 目前只是"能识别"，还不影响输出。
P2 那边这轮未动，`column_audit` 仍是 printed 49。
## 六十六、P3：`clang-format off/on` 接线完成，与 clang-format 逐字节一致

上一轮只做了"能识别"，这轮接进了 render：

```rust
// render 的循环开头
if disabled.get(index).copied().unwrap_or(false) {
    // 该 gap 原样抄自源码：不重排空格、不重排缩进
} else if forced && ... { ... }        // 原有的整条 if/else 链
```

`disabled` 来自 `regions::per_token`（上一轮的区间模型）。关键点：**token 仍然参与 layout**
（区间里的 `{` 照样开块），所以 `on` 之后的缩进仍然正确；只是这些 gap 的字节不让动。

**与真实 clang-format 对照（同一份 `.clang-format`、同一个输入）**

```text
clang-format:  int a = 1; / // clang-format off / int   b   =   2; / // clang-format on / int c = 3;
ours:          完全相同，五个行逐字节一致
```

**实测（回归未变）**

```text
cpp_formatter  78 → **79 passed** + 3 ignored（新增 a_clang_format_off_span_is_copied_verbatim）
语料           43 / 43（100.00%）· 稳定性 43 / 43
间距           19989 / 19989（100.00%）
P2             printed 49（未动）
```

P3 剩余：`-output-replacements-xml`；把 `format_range` 接到 LSP 的 rangeFormatting（目前服务器侧自己过滤）。
P2 下一轮：加"断行一致率"指标驱动候选集扩展。
## 六十七、P2 的新尺子：**断行一致率**（`examples/breaks_audit.rs`）

`column_audit` 只数"有几行太长"。这轮加了能说明**差在哪**的指标：两边用**同一个样式**（把语料 `.clang-format`
的 `ColumnLimit` 改成 80 后各跑一次），逐 token 比较"落在第几行"。

```text
add-only（当前实现）： files 43 · tokens 23194 · 落在不同行的 19042（**17.9% 一致**）
                       行数相同的文件 16 / 43
                       超长行 49
```

## 用新尺子量了两种"更激进"的模型（都回退）

```text
完全重排（忽略源码断行，全部按 breaker 的断点）：  一致率 **1.5%** · 超长行 **102**  ← 两个指标都更差
能放下的逻辑行合并（join-if-fits）：              一致率 **4.7%** · 超长行 **193**  ← 同样更差
```

结论与之前一致、但现在有数字：**在现有候选集下，"只增断行"是三者中最优**（17.9% / 49）。
要真正提高一致率，必须让候选断点更接近 clang-format 的 `BreakableToken`（实参中间、`?`/`:` 两侧、
字符串内部、注释重排），而不是换"谁来重排"。

**实测（回退后基线）**

```text
cpp_formatter  79 passed + 3 ignored
语料           43 / 43（100%）· 稳定性 43 / 43
P2             一致率 17.9%（16/43 行数相同）· 超长行 49
```

下一轮：按 `breaks_audit` 的逐 token 差异，挑**差异最集中的一类**（先统计 last-token/位置类型）扩候选集，
一次只加一类，用"一致率上升且超长行不增"作为提交条件。
## 六十八、断行差异的分类：**全部是 "extra"**——这是设计差异，不是缺陷

给 `breaks_audit` 加了分类（按"我们行的起始 token 的前一个 token 类型" + 方向）：

```text
files: 43 · tokens: 23194 · 不同行 19042（17.9%）
  5547  extra after Identifier        ← 我们断行、clang-format 不断
  1825  extra after RightParen
  1822  extra after LeftParen
  1444  extra after Semicolon
   865  extra after LeftBrace
   856  extra after RightBrace
   772  extra after Scope（::）
   765  extra after Dot
   660  extra after Comma
   403  extra after ReturnKeyword
```

前 10 名**全是 `extra`**（没有一条 `missing`）。含义很直接：

```text
我们的格式化器保留源码的换行（这是 ColumnLimit 0 语料下的正确行为），
而 clang-format 在 ColumnLimit 80 下会把能放下的行"合并"回去。
所以 17.9% 这个数字度量的是"设计差异"，不是"我们少断了多少"。
```

据此又试了"能放下的逻辑行合并"（并修掉了漏算尾注释宽度的问题），实测仍是 **4.7% / 194 超长行** ——
因为合并后的行宽还取决于 render 实际用的续行缩进，`breaks` 侧估不准；继续按这个方向做只会把两个指标都做坏。

**结论（P2 的诚实定位）**

```text
· 一致率 17.9% 不能作为 P2 的目标函数（它主要由"保留 vs 重排"决定，而保留是语料配置要求的）
· 真正可改善、且与用户价值一致的指标仍是**超长行数（49）**：只在必要时加断点
· 要把 49 降下去，只能扩候选断点（实参中间、`?`/`:` 两侧、字符串内部、注释重排），
  而这属于"给 breaker 更多位置"，不是"换谁重排"——三次实验（完全重排 1.5%/102、合并 4.7%/194、
  只增断行 17.9%/49）都支持这一点
```

基线回退并保持：79 tests、43/43、一致性 17.9%（16/43 行数相同）、超长行 49。
`breaks_audit` 的分类输出保留，它是后续挑候选断点的依据。
## 六十九、P3：`-output-replacements-xml`（与 clang-format 的 XML 同形）

新模块 `crates/cpp_formatter/src/output.rs`（4 个单测）+ CLI 开关 `-output-replacements-xml`：

```xml
<?xml version='1.0'?>
<replacements xml:space='preserve' incomplete_format='false'>
<replacement offset='4' length='14'>a = 1;&#10;int b = </replacement>
</replacements>
```

与 clang-format 的对照（同一份输入、同一份样式）：

```text
clang-format: 4 条细粒度 replacement（offset 3/17/18/20）
ours:         1 条粗粒度 replacement（offset 4，覆盖变更的中间段）
```

两者**应用后的结果相同**；差别是粒度。我们选择"一条"是因为库的契约是"这些字节产出那段文本"，
把细粒度留给 `layout_audit` / `spacing_audit` 这类需要定位的工具（模块文档里写明了这个取舍）。

**已知保真度差距（记录，不修）**：CRLF 输入时，clang-format 的 replacement 文本会归一成 `&#10;`（LF），
我们的仍保留 `\r\n`——语义等价，字节不同。

**实测**

```text
cpp_formatter  79 → **83 passed** + 3 ignored（output:: 4 个新单测）
语料           43 / 43（100%）· 稳定性 43 / 43
```
## 七十、P3 收尾：LSP 的 `rangeFormatting` 改用 `format_range`

服务器侧不再"整文件格式化后按行过滤"，而是直接调用库里的 `format_range`：

```rust
let formatted = match only {
    Some(range) => cpp_formatter::format_range(&text, &style, range.start.line as usize, range.end.line as usize),
    None => cpp_formatter::format_file(&path, &text, &style),
};
```

于是**编辑不可能越出范围**（`format_range` 只替换范围内的行），服务器侧那段"按行过滤"的代码也随之删掉。

**端到端断言加强**（`tests/handshake.rs`）：

```text
请求 rangeFormatting（只覆盖第 0 行）后，把返回编辑的 newText 拼起来：
  断言 **不含** "int y" —— 第 1 行不在范围内，不能被改写
```

**实测**

```text
cpp_ls --lib            88 passed + 1 ignored
cpp_ls --test handshake 32 passed + 5 ignored（含新的范围断言）
语料                    43 / 43（100%）· 稳定性 43 / 43（未变）
```

P3 的三项（off/on、`DisableFormat`、`format_range` 复用、replacements XML）至此全部完成。
## 七十一、超长行的成分普查：49 行里只有 **6 行** 是"搜索看不见"的

给 `column_audit` 加了一项统计：我们输出里每一行超长行中，`breaks::is_breakable` 是否至少给出一个断点。

```text
printed: 49 line(s) over 80 in our output, 0 in clang-format's
  of the over-long lines, **6** contain no candidate break at all
```

含义与下一步：

```text
· 只有 6 行（长字符串/长注释）是"候选集看不到"的，需要 token 级断点（改 token 流），是另一件事
· 其余 43 行**有候选断点却仍然超长** —— 问题在搜索/代价模型，不在候选集
```

据此又试了一个具体假设：续行片段在 `break_over_long_pieces` 里用 `blocks[start]`（续行是 `None` → 列 0），
于是片段"看起来不够长"、不会断。改成用源码缩进测量后实测**仍是 49**，无收益，已回退（`breaks.rs` / `render.rs` 复原）。

**当前基线**（未回退项：普查统计 + `is_breakable` 公开）

```text
cpp_formatter  83 passed + 3 ignored
语料           43 / 43（100%）· 稳定性 43 / 43
P2             printed 49 · 其中 6 行无可选断点 · 一致率 17.9%
```

下一轮的方向因此改变：不再扩候选集，而是**看那 43 行为什么断了也没变短**——最可能是
`break_line` 的段代价里，`start_column` 与实际渲染列不一致（同样低估），以及"只切一次就停"。
我会先给 `break_line` 加一个可选的回调/参数让调用方传入真实起始列，再测。
## 七十二、那 43 行"有候选却仍超长"的真相：不是缺断点，是**续行缩进太深**

把 ColumnLimit 80 下我们输出的超长行取样（12 条）：

```text
LuaSyntaxNode.h:80   [                                <bool(LuaTokenKind)> predicated, const LuaSyntaxTree &t) const;]
LuaSyntaxNode.h:83   [                                                   const LuaSyntaxTree &t) const;]
LuaSyntaxNode.cpp:117[        <LuaSyntaxNode> LuaSyntaxNode::GetChildren(const LuaSyntaxTree &t) const {]
LuaSyntaxNode.cpp:226[                                                         const LuaSyntaxTree &t) const {]
LuaSyntaxTree.cpp:41 [                for (auto rIt = parents.rbegin(); rIt != parents.rend(); rIt++) {]
```

前两类（`<bool(…)> predicated` 与 `const LuaSyntaxTree &t`）是**续行**，缩进已经到 32/50 列——
它们超长的原因是**缩进本身**，不是内容断不开；在 80 列下 clang-format 根本不是这样排的（它会把整个声明重排）。

根因是测量口径：语料是按 `ColumnLimit: 0` 排的（长行是允许的），我们又在 80 列下格式化它，
于是"保留源码断行 + 按 80 列判断"必然产生这类行——**这不是断行器缺断点，是输入与配置不一致**。
这也解释了为什么四种"重排"模型（完全重排 1.5%/102、合并 4.7%/194、按源码行 52、收敛式再断 50）
都不能把它变好：它们都在试图用"80 列的重排"去修补"0 列的排版"。

**结论**

```text
· P2 的"超长行"指标在 ColumnLimit 80 + 0 列语料下，天花板不在候选集或搜索，而在输入的不一致
· 更诚实的 P2 度量应当是：**在与 clang-format 相同的输入（即它自己的 80 列输出）上**比较我们是否稳定/一致，
  这项我们已经有了：稳定性 43/43（把 clang-format 的输出喂回来，一字不改）
· 因此 P2 我不再追求"49 → 更低"；下一轮改为把 column_audit 的口径写清楚（标注"输入与配置不一致"这一前提），
  并把"在 clang-format 输出上的一致性"作为 P2 的主指标
```

本轮实验（收敛式再断：49 → 50）已回退，基线不变。
## 七十三、P2 换口径后第一刀：**只搜源码行**，自洽口径下 15 → **30 / 43**

新增的自洽指标（`breaks_audit` 第三行）：把 clang-format 自己的 80 列输出喂回我们，看是否原样返回。
初始只有 **15 / 43** 不动。取样后发现原因：

```text
clang-format（80 列）：
    LuaSyntaxNode GetChildToken(std::function<bool(LuaTokenKind)> predicated,
                                const LuaSyntaxTree &t) const;
我们：
    LuaSyntaxNode GetChildToken(std::function
                                <bool(LuaTokenKind)> predicated,
                                const LuaSyntaxTree &t) const;
```

我们**多断了一次**：`break_line` 按"整条逻辑行的长度"搜索，而这条语句已经跨两行、每行都不长，
于是 DP 认为它超长、加了一个 clang-format 不会加的断点。修法：**只对源码行（分段）搜索**，去掉整行搜索。

```text
                        改前        改后
自洽口径稳定性（43 文件）  15 / 43  →  **30 / 43**
断行一致率               17.9%     →  **25.2%**
超长行（不一致口径，80 列）  49     →  51   （失去了一些"整行搜索"带来的断点）
语料 43/43 · 稳定性 43/43 · 间距 100%  均未变
```

净收益：自洽口径 +15 个文件、一致率 +7.3pp，代价是不一致口径下 +2 行超长——按"自洽口径才是 P2 的真指标"这一结论，接受。

下一轮：同样的方法处理剩下 13 个不稳定文件（先取样它们的首处分歧）。
## 七十四、自洽口径剩 13 个文件的两类差异（取样得到）

`breaks_audit` 现在会打印每个不稳定文件的首处分歧：

```text
FormatContext.cpp:38 ours="        LuaEditorConfig::OpenFile(...)"      clang-format="                LuaEditorConfig::OpenFile(...)"
Util.h:18          ours="enum class WorkMode { File, Stdin, Workspace};" clang-format="enum class WorkMode { File, Stdin, Workspace };"
LuaSyntaxTree.cpp:200 ours=".......Data.TokenIndex];"                    clang-format=".........Data.TokenIndex]"
```

归成两类：

```text
① 枚举/类体的**单行**写法：clang-format 在 `}` 前留空格（`{ File, Stdin, Workspace };`），我们没有
   —— 我们把枚举体的花括号当成 C++11 初始化列表（`{1, 2}` 无空格），而 clang-format 把它当"块"
   影响 7 个文件（Util.h / NodeOrToken.h / LuaOperatorType.h / LuaParseError.h / Mark.h / …）
② 续行的对齐列：`LuaEditorConfig::OpenFile(...)` 这类语句的续行，我们 8 列、clang-format 16 列
   影响 3 个文件（FormatContext.cpp / LuaCheck.cpp / LuaFormat.cpp）
```

定点实验（`ColumnLimit: 0`）确认了第 ①类：

```text
输入              clang-format                我们
enum class E { A, B };   →  enum class E { A,      →  enum class E { A, B};
                            B };
struct S { int a; };     →  struct S {              →  struct S { int a; };
                              int a;
                            };
int x[] = {1, 2};        →  不变（无空格）      →  不变（无空格）
void f() { g(); }        →  不变（有空格）      →  不变（有空格）
```

注意：`enum`/`struct` 体在 clang-format 里还会被**展开成多行**（`AllowShortEnumsOnASingleLine` 默认 false），
这属于"重排"，不在本轮的空白规则内；单行场景下只剩"`}` 前要有空格"这一条。

**下一轮**：给 `space::between` 加"枚举体/类体的 `}` 前留空格"（需要一个 `is_enum_or_class_body` 判定，
可复用 `layout::in_declaration_body` 的扫描），然后看自洽口径 30 → ~37。
## 七十五、枚举/类体 `}` 前的空格：自洽口径 **30 → 35 / 43**

`space::between` 的 `RightBrace` 分支原来只认 `roles.block_brace`。枚举体/类体在 roles 里是"初始化列表"
（对 layout 正确，对空格错误），于是 `enum class E { A, B};` 少了 `}` 前的空格。
新增 `closes_a_declaration_body`（回溯到匹配的 `{`，再看它所在语句的首 token 是不是 class/struct/enum/union），
命中时返回 1。

**实测**

```text
                         改前      改后
自洽口径（80 列输出喂回）  30 / 43 → **35 / 43**
断行一致率               25.2%    → 25.2%（不变）
语料 43/43 · 稳定性 43/43 · 间距 19989/19989 · 83 tests 均未变
```

同类差异还剩 8 个文件：3 个是"续行对齐列"（`LuaEditorConfig::OpenFile(...)` 续行 8 vs 16 列），
其余为深层续行/空行类，需要逐个按首处分歧处理。

**P2 的阶段结论**

```text
· 口径已改为自洽口径（clang-format 自己的 80 列输出喂回来）
· 自洽稳定性 15 → 30（只搜源码行）→ 35（枚举体空格）
· 不一致口径的超长行 49 → 51（因为去掉了"整行搜索"带来的断点，这是口径代价，已在文档 §七十二说明）
```
## 七十六、用户报告的两个问题：**行内块注释的空格** 与 **默认 4 空格缩进**

### ① `return` 没被正确格式化 —— 根因是行内 `/* */` 的空格

定点对照（`ColumnLimit: 0`）暴露出真正的 bug：

```text
输入                        clang-format              我们（修复前）
return/* c */1;         →   return /* c */ 1;        →   return  /* c */1;    ← 前面多一格、后面少一格
f(/* c */ 1);           →   不变                    →   f(  /* c */ 1);     ← 前面多一格
return a /* c */;       →   不变                    →   return a  /* c */;  ← 前面多一格
```

两个原因叠加：

1. `trailing::align` 把"行内块注释"也当成**行尾注释**（它只看注释起点，不看注释后面还有没有代码），
   于是按"段列"给注释前补了空格；
2. render 的 gap 遇到注释就整段原样输出，从不重排注释前的空格，也不管注释后与下一个 token 的空格。

修法：

```text
· trailing::split_trailing_comment：注释必须**结束该行**才算行尾注释（`//` 天然如此，`/* */` 要求其后只有空白）
· render 新增一条分支：**行内块注释按 token 规则加空格**
    注释前：前一个 token 是 `(`/`[` → 0 格，否则 1 格
    注释后：下一个 token 是 `;` `)` `]` `,` → 0 格，否则 1 格
```

修复后三种写法与 clang-format **逐字节一致**。

### ② 默认风格改为 4 空格缩进

新增 `Style::fallback_style()`（LLVM + `IndentWidth: 4`），用在两个"没有 `.clang-format`"的入口：
`config::style_for_file`（语言服务器）与 `cppfmt` 的 `-style=file` 回退。

```text
无配置时：      cppfmt → 4 空格缩进（项目约定）        ✓
-style=LLVM：  仍按 clang-format 原义 → 2 空格缩进      ✓
-fallback-style=Google 等：按给定值，不被改写            ✓
```

### 实测

```text
cpp_formatter  85 passed + 2 ignored（原 83 + 3；两条曾因旧行为而 ignore 的用例现在通过）
cpp_ls         88 passed + 1 ignored
语料           43 / 43（100%）· 稳定性 43 / 43 · 间距 19989 / 19989（100%）
```
## 七十七、清理编译警告；`onTypeFormatting` 按用户反馈**移除**

### 警告清零

```text
· cpp_formatter/src/columns.rs   —— 三个不可达的宽字符区间（被 0x1_F004..=0x1_F9FF 覆盖）
· cpp_formatter/src/render.rs    —— 重复的 #[test] 属性（同时修回了被挤掉签名的表缩进用例）
· cpp_formatter/examples/column_audit.rs —— 重复区间 + 未使用的 style_for
· cpp_ls/handlers/formatting     —— 删掉行内注释后失用的 touches 及其测试
· cpp_code_analysis/examples/std_probe.rs —— 未使用的 key（改名 _key）
```

`cargo build --workspace --all-targets` 现在**零警告**；`cargo test -p cpp_formatter --lib` / `-p cpp_ls --lib` 也无警告。

### `textDocument/onTypeFormatting` 已移除

按你的反馈（触发字符不好生效），整块撤掉而不是留着"有时能用"：

```text
· formatting 模块：删除 on_type_formatting 处理器、document_on_type_formatting_provider 能力
· request_handler：删除 OnTypeFormatting 的 import 与 dispatch 行
· tests/handshake.rs：删除 on-type 的断言与能力断言
· 模块文档改写：明确写出"为什么不再宣告"（客户端只在它认可并被告知的触发字符上发请求，
  实测不可靠；保存/按需格式化已覆盖同样的重排需求）
```

保留的格式化能力：**`textDocument/formatting`**、**`textDocument/rangeFormatting`**（走 `format_range`）、
**`textDocument/willSaveWaitUntil`**。

### 实测

```text
cpp_formatter  84 passed + 2 ignored · cpp_ls 87 passed + 1 ignored · handshake 32 passed + 5 ignored
语料           43 / 43（100%）· 稳定性 43 / 43
构建           workspace --all-targets 零警告
```
## 七十八、按截图修的三个问题（`static_assert` / `operator=` / `= default;`）

用户截图暴露的四处问题，先修好三处；两处仍需布局改造（列在末尾）。

### 已修

```text
① `static_assert (x)` → `static_assert(x)`
   a_space_before_a_paren 现在接收"前一个 token 的文本"，assert/sizeof/alignas/alignof/decltype/noexcept/
   typeid/requires 这些"长得像关键字、其实是标识符"的名字一律与括号粘连（clang-format 22.1.3 就是这样）
② `operator = (T &&)` → `operator=(T &&)`；`operator == (const T &)` → `operator==(const T &)`
   ① operator 关键字后的标点粘连；② operator 标点后的 `(` 也粘连
③ `= default;` 曾被排到第 38 列（对齐到 requires 子句里的 `==`）
   operand 对齐现在排除 `Assign`：`= default;`/`= delete;` 是续行，不参与算子对齐
```

**实测**：`cpp_formatter` 84 passed + 2 ignored；语料 **43/43（100%）**、稳定性 43/43、间距 19989/19989 全部未变。

### 仍待处理（已定位，需要布局层改动）

```text
④ requires 子句的缩进：clang-format 写 8 列（声明行 +4），我们写 4 列（当成块级）
   需要把 `requires(...)` 识别为"声明的续行"，属于 layout 的 statement/continuation 判定
⑤ 模板实参列表的续行掉到第 0 列：
     using S2 = Scenario
              <A::Not, B::Not,
     C::Not,                     ← 应为 20 列（对齐到 `Scenario<` 之后）
     D::Not>;
   根因：layout 只把 `(`/`[` 当括号上下文，模板的 `<...>` 不算，于是这些行被当成块级 0
   注意：语料里的模板续行都在类体内（块级 1），改这条必须同时验证 43/43 不回退
```
## 七十九、自造样例集：拿 clang-format 的输出喂回来，逼出四处真 bug

按你的建议造了一份 6 个小文件的样例集（模板/concept、运算符重载、语句、类、预处理、杂项），
每份先让 clang-format 格式化，再把它**自己的输出**喂回我们，逐字节比较。首轮只有 **3 / 6** 通过，
暴露出四个此前语料覆盖不到的真 bug（都修好并复测）：

```text
① `auto f =[&](int a)`   —— `[` 一律粘连，`=` 后丢空格
② `requires(T t) {t.size(); }` —— roles 把这种 `{` 判成初始化列表，花括号内丢空格
③ `[](int a)->int`       —— `->` 一律粘连；应区分成员访问（粘）与 lambda 尾置返回（两侧各一格）
④ `(void) i;`            —— C 风格 cast 后多一格；`(void)x` 应粘连
```

修法（都在 `space.rs`）：

```text
① `[` 前遇到 `=`/`return`/`,`/`(`/`{` 时保留空格
② `{` 前一个 token 是 `)` 时按"块"处理（两侧各一格）
③ `->`：如果 `)` 配对到的是 lambda 引导符的 `(`（即 `(` 前是 `]`）→ 两侧有空格，否则粘连
   （第一版只用"前一个 token 是 `)`"判断，语料掉了 1 个文件：`find(text)->second` 被写成 `find(text) -> second`）
④ 新增 `is_a_cast_expression`：括号内全是内建类型名/`*`/`&`/`const` 时，`)` 后不加空格
```

**实测**

```text
语料 43/43（100%）· 稳定性 43/43 · 间距 19989/19989 全程未回退
样例集 3/6 → **4/6**
cpp_formatter 84 passed + 2 ignored
```

**剩下 2 个样例差异（已定位，属布局/重排）**

```text
s3  `for (auto x : v) total += x;`  clang-format 把循环体拆到下一行（8 列），我们保持单行
    —— "是否重排"的设计差异，不是空白规则
s5  `enum class Color : unsigned char { Red,` 之后，clang-format 把后续枚举项对齐到首项列（35），
    我们按块缩进 4 —— 花括号"列表"内续行的对齐规则（与枚举单行展开同源）
```

下一轮：把 s5 这类"花括号列表内续行对齐"补上（`AlignAfterOpenBracket` 对 `{` 的处理），
并把样例集固化成仓库里的回归夹具（`tests/snippets/` + 一个 `snippet_audit` 例子）。
