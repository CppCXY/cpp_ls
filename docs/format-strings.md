# 格式字符串的高亮：`{...}` 与 `%...`

> **状态：已实现并实测（Phase 1 + Phase 2）。** 这份文档原本是"后面去实现"的方案；现在它是**实现记录**：
> 每一节末尾的「实测」是这一轮跑出来的数字，与方案不一致的地方都写明了为什么改、量到了什么。
> 还剩 Phase 3（相邻字面量拼接、hover、诊断），见 §9。
>
> ```text
> scanner   crates/cpp_code_analysis/src/sema/format.rs         13 个单测 + 表驱动
> locator   同上（literals_in）                                  15 个会话级测试
> 接线      crates/cpp_ls/src/handlers/semantic_token/format.rs  3 个编码级测试
> 配置      [highlight]                                          project/config.rs
> 探针      crates/cpp_ls/tests/format_status.rs                 真工程、真服务器、读回线上数据
> ```

---

## 1. 要画什么

把**格式字符串内部**的子区间单独着色，其余部分保持客户端给的字符串色：

```cpp
std::println("值 = {:.3f}，宽度 {:{}}  {{ok}}", x, w);

printf("%-8.2f|%*d|%1$s|%[a-z]%%", y, w, s, buf);

scanf("%3d %[a-z] %n", &n, name, &used);
```

期望的 token（`formatSpecifier` 是整段，`formatArgument` / `formatConversion` / `formatEscape` 是其中的子区间）：

| 写法 | 子区间 | 类型 |
|---|---|---|
| `{:.3f}` | `:` `.3` `f` 所在的整段 | `formatSpecifier` |
| `f` | 转换字符 | `formatConversion` |
| `{w}` / `{0}` / `%1$` | 实参引用 | `formatArgument` |
| `{:{}}` | 内层 `{}` | `formatSpecifier`（`DynamicWidth`） |
| `{{` `}}` `%%` | 转义 | `formatEscape` |
| `%-8.2f` | 整段 | `formatSpecifier`，其中 `f` 是 `formatConversion` |
| `%*d` | `*` | `formatArgument`（宽度来自实参） |
| `%[a-z]` | 整段 | `formatSpecifier`，`[a-z]` 是 `formatConversion` |
| `%n` | 整段 | `formatSpecifier`（`%n` 不取地址，见 §7） |

**非目标（本阶段不做）**：

* 格式串与实参的**类型/个数校验**（那是诊断，见 §9 phase 3；复用同一份 scanner）；
* 改变客户端自己的字符串语法高亮（TextMate 注入等，见 §2 为什么否决）；
* 运行时字符串（`std::vformat`、变量、宏参数、`std::runtime_format`）——不画，因为编辑器里没有可画的字面量。

---

## 2. 为什么在 server 的 semantic token 层

现有挂点（都不用新增协议能力）：

```text
crates/cpp_ls/src/handlers/semantic_token/mod.rs      已注册 semanticTokens/full，负责 delta 编码
crates/cpp_ls/src/handlers/semantic_token/builder.rs  legend（TypeKind）、Modifier、push_range
```

`builder.rs` 已经解决的、本功能必须复用的事情：

* `push_range(start, end, TypeKind, Modifier)` —— **首占先得**；跨行的区间按已有规则拆/丢；客户端画不出的
  类型直接不发（`TypeKind::VariableTemplate` 就是"自定义类型 + 回退"的先例）；
* 坐标是**文件坐标**，且 LSP 的 `character` 是 UTF-16 码元——`encode` / `position_at_offset` 已经处理，
  本功能**不得**自己换算；
* `Builder::push_range` 在 `start` 上首占先得，所以各 pass 的先后有语义；格式串在字符串内部，与
  "名字"那几层不会撞，顺序上放在最后即可。

被否决的方案：**VS Code 的 injection grammar / TextMate 注入**。

```text
· 只服务一个客户端（neovim、别的编辑器看不到）；
· 同一份数据将来要给 hover（"%.2f 是什么"）、inlay、诊断（格式串与实参不匹配）用，
  grammar 里再写一遍，两份规则必然漂移；
· 服务端已经有 legend、有 range 映射、有 delta 编码，接线的成本远低于新增一套语法。
```

---

## 3. 三层

### 3.1 Scanner —— 纯函数，`crates/cpp_code_analysis/src/sema/format.rs`

不依赖 LSP、不依赖索引。输入"字面量文本 + 种类"，输出区间：

```rust
pub enum FormatKind { Std, Printf, Scanf }

pub enum PieceKind {
    Specifier,      // {…} / %… 的整段
    Argument,       // {0} {w} %1$ %*（宽度/精度取自实参）
    Conversion,     // f d s x ? [a-z] …
    Escape,         // {{ }} %%
    DynamicWidth,   // 嵌套的 {}（宽度或精度）
}

pub struct Piece {
    pub kind: PieceKind,
    /// 在字面量 **token 文本**内的字节偏移（不是文件偏移）。
    pub start: usize,
    pub end: usize,
}

pub fn pieces_in(literal: &str, kind: FormatKind, out: &mut Vec<Piece>);
```

约束：

* **不 panic、不越界**：未闭合的 `{`、`%` 在末尾、`%[` 没有 `]` 都要停在字面量末尾并只画已确定的部分；
* **先处理 C++ 字面量自己的转义**：`\"`、`\\` 是 token 文本里的两个字节，不能让第二个字节被当成 `%`
  或 `{`；raw string（`R"(...)"`）里没有反斜杠转义，直接扫；
* 输出必须**升序、互不重叠**（`push_range` 首占先得，重叠会让后一个被丢掉）；
* 复杂度 O(字面量长度)，一次分配（`out` 复用）。

**实现时改掉的两处**（都是写代码时才发现方案里没写死的事）：

1. **`{}` 是一件，不是两件**。`{` 和 `}` 同属 `Specifier` 且相邻，所以追加时**合并成一段**（`push` 里
   `last.kind == kind && last.end == start` 就延长）。不合并的话 `{:.3f}` 会变成 5 个 token、`%-8.2f` 6 个，
   客户端上同一块颜色被画六遍。所以 §1 里"整段 + 子区间"的**包含关系**在输出上是**相邻的不同段**：
   `{:.3` 是 `Specifier`、`f` 是 `Conversion`、`}` 是 `Specifier` —— 看上去和"整段里挖一块"一样，但每个
   客户端都能画（不依赖 `overlappingTokenSupport`）。
2. **字面量的外壳要自己剥**：前缀是十种写法之一（`u8R"` `LR"` `uR"` `UR"` `R"` `u8"` `L"` `u"` `U"` `"`），
   不能写成"跳过字母"—— `u8` 里有数字。raw string 的体在 `(delimiter` 和 `)delimiter"` 之间，
   所以结束位置用**整段后缀**判断，而不是找最后一个 `)`。

### 3.2 Locator —— 回答"这是不是格式串、哪种、第几个实参"

**这一节和方案不一样，而且是为了一个真实的解析缺陷改的。**

方案说"找到包含该字面量的 `CallExpr`，复用 `index/project.rs` 的走法"。实测不行：

```cpp
std::format("...", x)      CallExpr            ✓ 树里有
std::println("...", x);    Declaration         ✗ 树里没有调用
printf("...", x);          CallExpr            ✓
ns::xyz("f", 1);           Declaration         ✗
```

**语句以"限定名 + `(`"开头时，这个解析器读成声明**（C++ 的 most-vexing-parse 形状）。`std::sort(v.begin(),
v.end());` 也一样 —— 这就是 siganture help、参数个数检查一起失效的原因，是解析器自己的缺陷（记在
`docs/remaining.md`）。同一段源码有**两个读法**，而格式串两个读法里都在，所以这一层走的是**token 流**：

```text
literal 的 token 下标 i
  i-1 必须是 `(` 或 `,`；i+1 必须是 `,` 或 `)`     —— 整个实参就是这一个字面量
  向前配平括号找调用自己的 `(`（`)`/`]`/`}` 加深、`(`/`[`/`{` 减浅；`;` 在深度 0 结束）
  `(` 前面是标识符链（`a::b::c`），前面是 `.`/`->` 就是成员调用
  从 `(` 到 i 数深度 0 的逗号 —— 就是实参下标
```

实测（`crates/cpp_code_analysis/tests/format.rs`，15 个）：`std::println` 那句读得出来，`printf(a + "%d")`
读不出来，`printf("%s", "text")` 只画第一个，`log.printf(…)` 按方法名画、`log.format(…)` 不画。

**名字怎么判定**（`the_format_kind_of`），三道，每道答不了就**拒绝**而不是猜：

```text
1. 名字    THE_TABLE 里有没有：format / format_to / format_to_n / formatted_size / vformat / vformat_to
           print / println / vprint / vprintln……  printf 家族（含 fprintf/snprintf/_s/宽字符/`syslog`）……
           scanf 家族（含 `%[` 的那几个）
2. 作用域  裸名（`printf`）按名字认——它是 C 库的约定名，没有命名空间可撞；
           `format` 是 `std`/`fmt` 的，所以裸名必须**解析**到 `std::` 或 `fmt::` 才算
3. 拼写    `std::`、`::`、`fmt::`、`fmt::v11::` 是库的；其它限定名是项目自己的，只有 `[highlight]` 能说它是包装
```

**解析用的是绑定，不是 `definitions`**：`definitions` 回答"跳到哪里"，它的 `DeclFact::scope` 填的是**光标处
写的限定符**（裸名就是 `None`），实测 `using namespace std;` 和 `using std::format;` 两种写法都得到
`scope: None`。这里要的是"声明坐在哪个命名空间"，所以走 `sema::resolve::definition_at` 拿到 `Binding`，
再 `ScopeTree::qualified_name_of(binding.scope)`。实测：`using namespace std;` + `format("{}", 1)` 认出来了，
`using namespace app;` 的同一个拼写没认出来。

**还剩一个答不对的形状**：`using std::format;` 之后裸写 `format(...)` —— 那个名字绑在 using 声明**自己**
身上（和 go-to-definition 落点一致），所以拼写解析成文件作用域的 `format` 而被拒绝。`using namespace std;`
是对的，限定写法的 `std::format` 不需要解析。要修得跟着 using 声明走一层，记在 §9。

### 3.3 LSP 映射 —— `crates/cpp_ls/src/handlers/semantic_token/format.rs`

```text
Session::format_literals(view, config)          // 分析层：文件坐标 + 已经切好的 pieces
    ↓  findings: Vec<FormatLiteral { range, kind, pieces }>
format::draw(builder, &literals)                // 每个 piece 先要自定义类型、被拒再要回退类型
    ↓
builder.push_range(start, end, kind, Modifier::none())   // 既有链路：UTF-16、排序、首占先得
```

* 文件坐标 → `push_range` → `encode` 的 UTF-16 转换，是既有链路；多字节 fill 字符（`{:宽>8}` 的 `宽`）
  和中文格式串都靠它，**不要自己算 character**；
* **只认写文本**：`view.written_text().is_some()` 就是"这是渲染"，`format_literals` 直接返回空。
  字面量本身不会被宏展开，但它后面的东西会被搬走，所以渲染坐标不是文件坐标；
* 这一趟放在 `encode` 的**最后**（名字、关键字、模块、doc 命令之后）：它知道得最少，首占先得下应该最后说话。
  实际不会撞 —— 每个 piece 都在字符串字面量里面，那里没有名字也没有关键字。

---

## 4. 文法（三张表）

### 4.1 `Std` —— `std::format` / fmt

```text
literal   := ( escape | field | other )*
escape    := "{{" | "}}"
field     := "{" [ arg ] [ ":" [ [fill] align ] [sign] ["#"] ["0"] [width] ["." precision] ["L"] [type] ] "}"
arg       := digits | identifier            // {0} {w} {name}（fmt 允许具名）
width     := digits | "{" [arg] "}"         // 动态宽度，内层标 DynamicWidth
precision := digits | "{" [arg] "}"
align     := "<" | ">" | "^"
sign      := "+" | "-" | " "
type      := "a" | "A" | "b" | "B" | "c" | "d" | "e" | "E" | "f" | "F" | "g" | "G"
           | "o" | "p" | "P" | "s" | "x" | "X" | "?"
```

* `fill` 是**一个字符**（可多字节 UTF-8），不能是 `{` 或 `}`；
* 嵌套 `{}` 只允许一层；内层出现 `:` 时按普通字符处理（标准不允许，不画错即可）；
* `{:?}`（C++23 debug）与 `{:L}`（locale）按上表归类；
* 只有 `{` 没有 `}`：画到字面量末尾，不 panic。

### 4.2 `Printf`

```text
conversion := "%" [ position "$" ] [flags] [width] ["." precision] [length] specifier
              | "%%"
flags      := ( "-" | "+" | " " | "#" | "0" | "'" )*     // glibc 另有 "'"、"I"
width      := digits | "*"                               // "*" 消耗一个实参 → Argument
precision  := digits | "*"
length     := "hh" | "h" | "l" | "ll" | "j" | "z" | "t" | "L"
specifier  := "d i o u x X e E f F g G a A c s p n m"    // glibc 的 %m
```

* `%1$s` 的 `1$` 是 `formatArgument`；
* `%*d` 的 `*` 是 `formatArgument`；
* `%n` 不进 §7 的"取地址"规则（`%n` 要指针，但本阶段不做类型判断）；
* 末尾孤立的 `%` 不画。

### 4.3 `Scanf`

```text
conversion := "%" [ "*" ] [width] [length] specifier | "%%"
specifier  := "d i o u x X e E f F g G a A c s p n [" ... "]"   // 无 flags、无 precision
```

* `%[` 的 scanset：`]` 出现在**首字符**时是内容；`-` 是范围；结束的 `]` 之后不能再有字符；
* `%*` 是抑制赋值（不取地址），按 `formatArgument` 画；
* `%n` 不消耗输入。

---

## 5. legend 与配色

`TypeKind` 原本有 17 项（`VariableTemplate` 是第 17 个，编号 16）。这一轮**追加 5 项**（`to_u32` 的顺序就是
线上编号，插在中间会让所有后面的编号错位），legend 现在 **22 项**：

| 编号 | 类型 | 画哪一段 | 客户端不认识时的回退 |
|---|---|---|---|
| 17 | `formatSpecifier` | `{:.3` / `%-8.2` / `[` 里的 `{:` 这样的段 | `regexp` |
| 18 | `formatArgument` | `0` `name` `1$` `*` | `parameter` |
| 19 | `formatConversion` | `f` `d` `s` `[a-z]` `?` | `keyword` |
| 20 | `formatEscape` | `{{` `}}` `%%` | 不给（字符串本来就是这个颜色） |
| 21 | `regexp` | —— 只是上面第 17 项的回退，本服务器自己不发 | —— |

回退由 `builder.rs` 的"客户端画不出这个类型就不发"机制实现（`variableTemplate` 的注释里写的就是这个）：
`format::draw` 先要自定义类型，`push_range` 答 `false` 再要回退类型。

**这一条从"风险"变成了"实测出来的主路径"**：VS Code 和 neovim 在 `initialize` 里报的都是**标准类型表**，
所以自定义的四个名字**永远不会被发出去**——真的到用户屏幕上的，就是 `regexp` / `parameter` / `keyword`
三个回退（实测见 §8）。这不是退路而是当下的默认配色；自定义类型是给"主题愿意配"的客户端留的上限。

客户端侧只需要主题配合，不替用户改主题：

```jsonc
// VS Code：settings.json。键就是 legend 里的名字，不必等客户端广告它
"editor.semanticTokenColorCustomizations": {
  "rules": {
    "formatSpecifier":  { "foreground": "#B58900" },
    "formatConversion": { "foreground": "#268BD2", "bold": true },
    "formatArgument":   { "foreground": "#D33682" },
    "formatEscape":     { "foreground": "#93A1A1" }
  }
}
```

```lua
-- neovim
vim.api.nvim_set_hl(0, "@lsp.type.formatSpecifier", { link = "@string.special" })
vim.api.nvim_set_hl(0, "@lsp.type.formatConversion", { link = "@keyword" })
vim.api.nvim_set_hl(0, "@lsp.type.formatArgument", { link = "@parameter" })
vim.api.nvim_set_hl(0, "@lsp.type.formatEscape", { link = "@string.escape" })
```

但**要让服务器真的发这四个名字**，客户端的 `tokenTypes` 里得有它们（见上：两个客户端默认都不报）。
`tests/format_status.rs` 的 `CPPLS_FORMAT_CLIENT=full` 就是这个形状的实测。

**"两个客户端默认都不报"是查出来的，不是猜的**：`vscode-languageclient` 在 `initialize` 里发的是标准表
（从装好的 `cppcxy.cppls` 的 `dist/extension.js` 里读出来的；同一段还写着 `multilineTokenSupport = false`、
`overlappingTokenSupport = false`）。第二条顺带证明 §3.1 那个决定是对的 —— 这个客户端明确说自己不支持重叠
token，所以"整段包着子段"只能画成相邻的段。

所以测试工程 `C:\Users\zc\Desktop\cpp` 的 `.vscode/settings.json` 里配的是 `regexp`，不是 `formatSpecifier`：
本服务器**只**在格式串的 field 上发 `regexp`，所以那条规则实际就是"格式串语法"的规则。`parameter` /
`keyword` 不动 —— 主题本来就有颜色，再配一遍会把文件里真的参数和关键字一起改掉。

---

## 6. 配置

放在 `.cppls.toml`，与 `[hover] enable` 同一个读取路径（`config.rs` 的 section 结构）：

```toml
[highlight]
format_strings = true              # std::format / print 家族 + fmt:: 的
printf = true                      # printf 家族
scanf = true                       # scanf 家族
extra = ["mylib::logf"]            # 项目自己的包装，printf 语法，格式串在第一个实参
extra_format = ["mylib::log"]      # 项目自己的包装，std::format 语法
```

* **默认全开**：`.cppls.toml` 里的 `None` 就是开 —— "这个字符串是格式串"是能从文本证明的事实，不是口味，
  所以不像诊断那样要项目点头；
* 三个都关（且两个列表为空）时这一步完全不跑：`FormatConfig::is_off()` 在 handler 边缘判，scanner 不知道
  配置存在；
* **分成两张表**是因为一个列表只能说一种语法：`mylib::logf` 收 printf 的 `%d`，`mylib::log` 收 `{}`，
  同名列没法两全；
* `extra` 匹配的是**解析后的限定名**（`definition_at` 拿绑定、`qualified_name_of` 拼作用域），所以
  `using mylib::logf;` 之后裸写 `logf(…)` 也算，而项目里另一个同名的 `logf` 不算。

---

## 7. 正确性清单（逐条对照，✓ 是实测过的）

```text
 1. UTF-16 码元：一律走 push_range/encode，不自己乘除；                   ✓ 中文格式串的用例
 2. 先跳 C++ 字面量自己的转义（\" \\），raw string 不跳；                 ✓ format_scanner
 3. {{ }} %% 是转义，不消耗实参；                                        ✓ 且相邻的会合并成一段
 4. 嵌套 {} 只允许一层（动态宽度/精度），内层标 DynamicWidth；             ✓ {:{}} → {:{ | {} | }
 5. { 未闭合、% 在末尾、%[ 没有 ] → 画到字面量末尾，绝不越界；            ✓ 6 个用例
 6. scanf 的 %[ 要吃到 ]；%] 第一个 ] 是内容；%n、%* 特殊但本阶段不判类型；✓ %[]] / %[^,] / %[abc
 7. 相邻字面量拼接（"a" "%d"）→ **phase 3**，现在整段跳过（不是画半个）；   ✓ 有用例钉住
 8. 宏包一层（#define LOG(f) printf(f)）→ 调用点不是字面量，跳过；         ✓ a + "%d" 那个用例
 9. 只在写文本上做（view.written_text().is_some() 直接返回空）；            ✓
10. 每文件 token 上限：**没做**，因为实测一个文件最多几十个 piece（§8）；
11. 输出升序、不重叠；                                                    ✓ 单测 + 编码级断言
12. 配置关闭时零成本；                                                    ✓ is_off() 在扫 token 之前
```

---

## 8. 测试与度量（实测）

```text
A. scanner 单测 —— crates/cpp_code_analysis/tests/format_scanner.rs   10 个测试，80+ 例
   {:d} {:>10.3f} {:+08.2f} {:?} {:L} {:{}} {0:{1}.{2}f} {{{}}} {{ok}} 中文 fill
   %d %.*f %-8.2f %+05d %#x %1$s %*d %zu %lld %Lf %n %m %% %y %5 %.2
   %[a-z] %[^,] %[]] %[abc %3d %ld %-d
   R"(\{d})" R"xy({{)xy" u8/L/u/U 前缀 未闭合的 { 和 %
   还有一条：每个 piece 都在字面量内、递增、char 边界、非空

B. locator —— crates/cpp_code_analysis/tests/format.rs               15 个测试
   std::format / std::println（语句形状）/ printf / fprintf / snprintf / scanf / sscanf
   std::print(stream, …) 与 std::print("…", "text")（第二个串不是格式串）
   fmt::print / fmt::v11::format / log.printf / app::format（不画）
   using namespace std 解析成功 / using namespace app 不画 / extra / 三个开关

C. 编码 —— crates/cpp_ls（semantic_token 的 unit test）                3 个测试
   三段文法各一行的 fixture，按客户端的方式解码后断言绝对偏移 + 类型号
   只认标准类型的客户端 → regexp/parameter/keyword，escape 不给
   关掉配置 → 字面量里一个 token 都没有
```

**真工程探针**（`crates/cpp_ls/tests/format_status.rs`，起真服务器、真 `initialize`、读回线上数据）：

```text
C:\Users\zc\Desktop\cpp\main.cpp          8 token，其中 1 个在字面量里
  std::format("Hello, {}!", "world")      standard 客户端 → regexp "{}"
                                          full 客户端     → formatSpecifier "{}"
  首个非 null 回答                        +3 ms（initialize 后第 2 次问）
  再次问                                  <1 ms

%TEMP%\cppls-format-fixture\main.c        38 token，17 个是格式 piece（full 客户端）
  "%-8.2f|%*d|%1$s|%%\n"   %-8.2 f  %  *  d  %  1$  s  %%        ← 逐段都对
  "%3d %[a-z] %n"          %3 d   % [a-z]   % n
  "%s\n"（fprintf 第 2 个实参）             % s

%TEMP%\cppls-format-fixture\format.cpp    21 token，10 个格式 piece（standard 客户端）
  std::format("Hello, {}!")                regexp "{}"
  std::println("value = {:.3f} width {:{}}")   regexp "{:.3" keyword "f" regexp "}"
                                               regexp "{:" regexp "{}" regexp "}"   ← 动态宽度
  printf("%d%%\n", 42)                     regexp "%" keyword "d"（%% 是 escape，回退不给）
```

**耗时**：

```text
小工程（上面三个文件）   semanticTokens/full 整个请求   3–9 ms
neovim message.c         114 476 字节 / 29 599 token / 88 个字符串字面量
                         格式串这一趟               36.5 µs   ← 20 次平均
                         classified_names           1.46 s    ← 既有的一趟，不是这一层
                         建 view                    18 ms
                         semanticTokens/full 整个    5.5 s（首次 19.6 s 是冷启动索引）
```

```text
$env:CPPLS_FILE = "E:\neovim\src\nvim\message.c"
cargo test --release -p cpp_code_analysis --test format -- --ignored --nocapture what_the_walk_costs
```

也就是说**这一层在真文件上是 36 µs，占不了一次回答的千分之一**；那个文件慢的是 `classified_names`
（1.46 s）—— 本来就存在的事，和格式字符串无关。所以**没有缓存、没有 token 上限**：不需要。

**实测到的一个小事故**（记在这里，因为它差点变成"功能没生效"的错结论）：真工程探针第一次读出来的 token
数是 0 —— LSP 的 `SemanticTokens.data` 是**一个扁平数字数组**（每 5 个数一个 token），探针把它当"五元组的
数组"读了。那个错误只存在于探针自己的解码器里（`handlers/semantic_token/mod.rs` 的测试一直按
`data.len() / 5` 数）。

**没有做的度量**：一次全量 `semanticTokens` 在 neovim（1337 文件）上的增量 —— 那要先把整个工程索引完，
留到下次与 §9 的 Phase 3 一起量。单个大文件的数字在上面。

---

## 9. 交付顺序（每步独立可验证、可回退）

```text
Phase 1  scanner(Std) + legend + LSP 接线 + 测试                        ✓ 已完成
Phase 2  printf / scanf 文法 + 实参位置表 + 配置                        ✓ 已完成
Phase 3  相邻字面量拼接、constexpr 值、hover（"%.2f = 定点两位小数"）、
         诊断（格式串与实参个数/类型不匹配，复用 pieces + 实参类型）      未做
```

Phase 1 的验收（`C:\Users\zc\Desktop\cpp` 的 `main.cpp`）**达成**：`std::format("Hello, {}!")` 的 `{}`
在服务器输出里是一个独立 token，关闭配置后完全恢复原样（§8 的数字就是这台机器上跑出来的）。

Phase 3 之前还剩一件小事：`using std::format;` 之后裸写 `format(…)` 不认（§3.2 末尾）。

---

## 10. 风险与取舍（实测结论）

* **自定义 token 类型**：LSP 允许，但**两个常见客户端都不在 `tokenTypes` 里报它们**，所以到屏幕上的是
  `regexp`/`parameter`/`keyword` 三个回退。这不是缺陷，是"宁可少画"的落地：主题不配也有区分，配了
  （并把名字加进 `tokenTypes`）才是四个精确类型。同一段代码两种客户端各跑一次的数字在 §8。
* **认识的函数有限**：默认只认标准名，项目的包装函数通过 `extra`/`extra_format` 加。实测的假阳性防护：
  `app::format("…")`、`record("…", 1)`、`printf(a + "%d")`、`printf("%s", "text")` 全都不画。
* **性能**：一次 `semanticTokens/full` 3–9 ms（整个请求，不只是这一层）。**不需要缓存**：最初担心的
  "每个字面量一次 `definitions`" 没有发生 —— 裸名才解析，而且 `definition_at` 是作用域链查找。
* **不重做库的检查**：格式串是否合法由编译器/库负责，我们只画。将来做诊断时也只在"能确定"的形态上报。
* **`swprintf` 故意不认**：C 的格式串在第 3 个实参、MSVC 在第 2 个，一张表没法两边都对，而猜错就是给
  实参上色。这就是"宁可少画"的具体一笔。

* **不重做库的检查**：格式串是否合法由编译器/库负责，我们只画；将来做诊断时也只在"能确定"的形态上报
  （字面量 + 可解析的实参类型）。
* **不追 C++26 的 `std::format_string` 变化**：若标准库把格式串改为 `format_string<Args...>` 构造，
  locator 的"第几个实参"表仍然成立（位置没变），不受影响。
