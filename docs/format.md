# 格式化器实现计划

**结论先行。格式化器只吃 lexer 的 token 流，不碰语法树。**

```text
1. 唯一输入是「写下来的那份文本」的词法结果 —— 语法树是**宏展开后**的，格式化不能以它为来源
2. 结构信息全部从 token 的**嵌套**里重建（括号栈），这和 clang-format 的做法同源
3. 两阶段：先决定「在哪里断行」（代价模型），再「输出空白」——中间不允许再读树
4. 验收不是「看起来像」，是三条机器可判的：幂等、token 守恒、与 clang-format 的 dif
```

---

## 一、为什么只能吃 lexer

`cook` 产出的 `RenderedCooked` 是**展开后**的文本：`#include` 被内联、宏被替换、注释被剥掉。格式化必须保留
用户写下的每一个 token（包括注释、包括 `#define` 的宏体），而展开后的树里这些**已经不存在了**。

```text
写下的文本 ──lexer──► token 流 ──► 格式化器 ──► 文本
                     ▲
                     └── 唯一来源：token 自带原文、种类、偏移、前面有没有空白
```

这不是退让，是 clang-format 本来的架构：它 `FormatToken` 的输入也是词法结果，语法树只用来**标注**某些 token
（`>` 是模板结束还是比较、`{` 是函数体还是初始化列表）。我们没有那条路（树是展开的），所以所有判断都必须
从 token 流本身得出 —— 下面的「启发式」一节说的就是这件事，以及它为什么够用。

## 二、数据模型

```rust
/// 一个 token，连同它**前面**的原始空白与注释 —— clang-format 的 `FormatToken` 同形。
struct FormatToken {
    kind: CppTokenKind,          // 复用 lexer 的种类，不另立一套
    text: Range<usize>,          // 在**写下来的文本**里的切片，零拷贝
    whitespace_before: Whitespace, // 有没有换行、几个空格、是不是在新行首
    /// 由分析阶段填：换行、缩进层级、与前一个 token 之间要不要空格
    break_before: Break,
    indent: i32,
    space_before: bool,
}

enum Break {
    None,            // 同一行
    Optional(penalty), // 可以断，代价
    Mandatory,       // 必须断
}
```

**「不展开的注释与预处理行」是普通 token**：`CppCommentToken` 和 `PreprocessorDirective` 已经在 lexer 里，格式化只
决定缩进与对齐，不改内容。

## 三、三个阶段

### 阶段一：UnwrappedLine 切分（只认 token）

把 token 流切成「语义上的一行」——`clang-format` 的 `UnwrappedLine`：

```text
行开始的判据（全部来自 token 本身，不需要树）
  · 上一个 token 是 `;`，且不在 `for (…)` 的括号里
  · 上一个 token 是 `{` 或 `}`（块开始/结束）
  · 上一个 token 是访问说明符后的 `:`
  · 上一行是预处理指令或注释独占行
  · `case X:` / `default:` / 标签
```

一条 UnwrappedLine 是**不保证单行放得下**的一行：`int f(int a, int b, …)` 有十个参数时仍是一条，断行在阶段二。

### 阶段二：代价模型（唯一的难点）

这是 clang-format 里 70% 的代码，也是我们唯一真正要「学」的部分。做法是**在 UnwrappedLine 上做带惩罚的贪心**：

```text
可行断点（可选断行）
  · 逗号之后（参数、实参、初始化列表）
  · 二元运算符之前
  · 赋值号之后
  · `->` / `.` 之后（成员链）
  · 开括号之后（若该括号内放不下）
  · 字符串字面量内部（clang-format 允许，我们第一阶段**不做**）

惩罚（越小越优先断）
  · 括号内最外层断点       低
  · 每深一层 +10
  · 二元运算符 +5，赋值 +2
  · 断在 `(` 之后 +30（比断在参数之间更难看）
  · 断在两个 token 都已超过列宽之后 —— 惩罚按超出量线性增长

求解
  1. 若整行放得下（列宽 ≤ ColumnLimit），全部 None
  2. 否则取「惩罚之和最小」的断点集合：行数不多时直接 DP（clang-format 同）
  3. 缩进 = 所在括号的**对齐列**（AlignAfterOpenBracket ✓）或 ContinuationIndentWidth
```

**与 clang-format 对齐的默认值**：`ColumnLimit 100`、`IndentWidth 4`、`ContinuationIndentWidth 8`、
`BreakBeforeBinaryOperators None`、`AlignAfterOpenBracket Align`、`BreakBeforeBraces Attach`、
`AllowShortFunctionsOnASingleLine All`、`SpaceBeforeParens ControlStatements`、`PointerAlignment Right`。

### 阶段三：输出

把 token 按 `indent` / `space_before` / `break_before` 拼回文本。**唯一写出的东西是空白**：

```text
assert 不变式：strip_whitespace(output) == strip_whitespace(input)   ← token 守恒
```

这条不变式就是「不丢代码」的机器判据，第三节的测试靠它。

## 四、需要「树」的地方，以及 token 层的替代

clang-format 用语法树标注 token；我们没有，于是每一条都要么有等价 token 判据，要么明确放弃：

| 判断 | clang-format 用树 | 我们怎么办 |
|---|---|---|
| `>` 结束模板还是比较 | 树 | **括号栈里的 `<` 计数 + 尖括号不可跨 `;`/`)` 闭合**；不确定时按比较（保守，不会多断行） |
| `{` 函数体 / 初始化列表 | 树 | 看 `{` 前一个 token：`=`、`,`、`(`、`return` ⇒ 初始化列表；`)`、标识符、`const` ⇒ 函数体 |
| `&`/`*` 是指针还是运算 | 树 | 前后 token 的种类：`*` 前面是类型名/`*`/`&` ⇒ 指针；前面是标识符/`)`/字面量 ⇒ 乘号 |
| `for (;;)` 里的分号 | 树 | 括号栈顶是 `(` 且它前面是 `for` ⇒ 不算语句结束 |
| 宏体里的逗号 | 树 | `#define` 行整行当**一个** token 序列保留，不参与断行 |

**这五条是全部**。写下来是为了后面每遇到一个反例，都能一眼看出它是这五条里的哪一条的误判，而不是「格式化器乱来了」。

## 五、文件组织

```text
crates/cpp_format/
  src/
    lib.rs            // format(text, &FormatStyle) -> String，以及 range 版本
    token.rs          // FormatToken，从 lexer 结果构造（注释/预处理都在）
    style.rs          // FormatStyle，默认值 = 第三节的 clang-format 默认
    lines.rs          // UnwrappedLine 切分（阶段一）
    break.rs          // 代价模型与 DP（阶段二）
    indent.rs         // 括号栈 → 缩进/对齐列
    emit.rs           // 阶段三
    tests/            // 幂等、token 守恒、golden
```

在 `cpp_ls` 里接 `textDocument/formatting` 与 `textDocument/rangeFormatting`：前者整文件，后者取**包含该范围的
UnwrappedLine 集合**（clang-format 也是这个粒度，否则会重排范围外的行）。

## 六、里程碑（每一步都能独立验收）

```text
M0  token 化            lexer → FormatToken，注释与预处理保留；token 守恒测试先立起来 ✓
M1  空白规范化          只处理「同一行内的空格」：不加/不删换行
                        ⇒ 幂等 ✓、token 守恒 ✓、对已格式化文件 diff 为空 ✓
M2  缩进                括号栈 → 每行 indentation；不管断行
                        ⇒ 对 EmmyLuaCodeStyle 缩进全对（人工抽查 20 个文件）✓
M3  断行：括号内        参数/实参/初始化列表；ColumnLimit 生效
                        ⇒ 与 clang-format 的 diff 行数开始下降（记录基线）✓
M4  断行：运算符        赋值、二元、成员链
M5  对齐               连续声明、行尾注释、`case` 标签；`AlignConsecutive*` 选项
M6  预处理              `#if` 的缩进、宏体的保留、`#include` 排序（可选）
M7  range 格式化 + LSP  接进 cpp_ls；与 clang-format 的 C 库做端到端比对
```

**M1 与 M2 加起来就是「可用的格式化器」**：绝大多数用户抱怨的是缩进和空格，不是断行。断行（M3/M4）是长尾。

## 七、测试

```text
① 幂等       format(format(x)) == format(x)                       —— 对语料库全量跑
② 守恒       strip_whitespace(format(x)) == strip_whitespace(x)   —— 一个 token 都不能丢
③ 不动       format(x) == x  对已格式化的文件（clang-format 的输出）
④ 差分       与 clang-format -style=file 的输出做行级 diff，**记录数量并只允许下降**
⑤ 语料       EmmyLuaCodeStyle（1,191 文件）＋ STL 头（宏与模板最密）＋ 一个宏密集的 Windows 头
```

③ 是这一套里最狠的一条：**对 clang-format 已经格式化过的文件，我们应当输出同样的东西**。做不到就先停在 ①②③。

## 八、明确不做（第一阶段）

```text
· 字符串字面量内部断行（clang-format 有，复杂且少见）
· 注释重排（reflow comments）—— 只缩进，不改字
· #include 排序 —— 有争议，且需要 include 搜索路径的知识
· 配置项的完整集合 —— 先只认 12 个常用项（见第三节）
· 与树的一致性检查 —— 格式化器不读树，这条不适用
```

## 九、风险

```text
① 断行的代价模型是唯一真正难的部分。缓解：M1/M2 先交付，断行单独迭代，差分基线只看「不上升」。
② token 层无法区分 `>` 的两种含义 —— 这是选择这条路的**固定**代价，缓解是「不确定时按比较」这条保守规则。
③ 宏体的格式化：`#define` 的续行符 `\` 必须保持，宏体内部的空格**不动**。
```

---

参考：clang-format 的架构说明（`FormatToken` / `UnwrappedLine` / `ContinuationIndenter` / `WhitespaceManager`），
以及 clang 的 `Format.h` 与 `ClangFormatStyleOptions.rst`（选项语义）。