# 格式字符串的高亮：`{...}` 与 `%...`

> **状态：方案，未实现。** 这份文档是给"后面去实现"用的，不是已完成的记录；里面每一处"现在有"
> 都指着一个可以用 `git grep` 找到的位置，每一处"验收"都写明了怎么量。
>
> 实现完成后，把 §8 的数字补上，并把 §10 里"风险"那一条改成实测结论。

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

### 3.2 Locator —— 用索引，回答"这是不是格式串、哪种、第几个实参"

放同一模块（将来诊断也要用），或 `index/format.rs`：

```rust
pub struct FormatSite {
    pub kind: FormatKind,
    /// 格式串在第几个实参（`printf` 的 buffer/stream 在前，`format_to` 的输出迭代器在前）。
    pub argument: usize,
}

pub fn the_format_site_of(
    session: &Session<…>,
    view: &FileView,
    literal: ReadingOffset,
) -> Option<FormatSite>;
```

步骤：

1. **找到包含该字面量的 `CallExpr`**。`index/project.rs` 已有"callee 属于哪个 `CallExpr`"的走法
   （`grep -n "The CallExpr a callee belongs to"`），直接复用，不要新写一套父子遍历。
2. **解析 callee**：取调用处的写法（`std::format`、`format`、`::printf`、`my::logf`），用
   `session.definitions(view, callee_offset)` 拿到声明；优先用**解析后的 qualified name**，解析不到再退回
   文本匹配（`std::` 前缀、`using std::format;` 的可见性由既有的
   `resolved_in_the_enclosing_scopes` 回答）。
3. **查表决定第几个实参**：

   ```text
   std::format / print / println / format_to_n          arg 0
   std::format_to                                       arg 1
   printf / fprintf / sprintf / snprintf                arg 0 / 1 / 1 / 2
   scanf / fscanf / sscanf / scanf_s / fscanf_s         arg 1 / 1 / 1 / 1 / 1
   ```

   这张表是 `const`，加 `.cppls.toml` 的 `extra` 让项目自己的包装函数进来（§6）。
4. **只要字面量**：实参必须是 `StringLiteral` token（`CppTokenKind::StringLiteral`，含 `L`/`u8` 前缀）。
   变量、宏、`std::runtime_format` 一律返回 `None`。

### 3.3 LSP 映射 —— `crates/cpp_ls/src/handlers/semantic_token/format.rs`

```text
for each StringLiteral token in view.tree.get_tokens():
    let file_offset = view.file_offset_of(token.start())?;        // 文件坐标
    let reading     = view.reading_offset_of(file_offset)?;
    let Some(site)  = the_format_site_of(session, view, reading) else { continue };
    let mut pieces  = Vec::new();
    pieces_in(token.text(), site.kind, &mut pieces);
    for piece in pieces:
        builder.push_range(file_offset + piece.start, file_offset + piece.end, kind_of(piece), Modifier::none());
```

* 文件坐标 → `push_range` → `encode` 的 UTF-16 转换，是既有链路；多字节 fill 字符（`{:宽>8}` 的 `宽`）
  和中文格式串都靠它，**不要自己算 character**；
* 只有 `view.written_text()` 可读时才做（写文本缺失就跳过；不要拿渲染文本扫，宏展开后的字面量位置
  对不上文件）；
* 每个文件一个 token 上限（例如 20 000），与 builder 的既有预算共用。

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

`TypeKind` 现在有 17 项（`VariableTemplate` 是第 17 个，编号 16）。新增 **4 项，必须追加在末尾**
（`to_u32` 的顺序就是线上编号，插在中间会让所有后面的编号错位）：

| 编号 | 类型 | 画哪一段 | 客户端不认识时的回退 |
|---|---|---|---|
| 17 | `formatSpecifier` | `{…}` / `%…` 的整段 | `regexp` |
| 18 | `formatArgument` | `{0}` `{w}` `%1$` `*` | `parameter` |
| 19 | `formatConversion` | `f` `d` `s` `[a-z]` … | `keyword` |
| 20 | `formatEscape` | `{{` `}}` `%%` | `string`（等于不画） |

回退由 `builder.rs` 的"客户端画不出自定义类型就丢弃/回落"机制实现（`variableTemplate` 的注释里写的就是
这个）；也就是说主题不认识 `formatSpecifier` 的客户端不会看到乱码或崩溃，只会保持原样。

客户端侧只需要主题配合，不替用户改主题：

```lua
-- neovim: ~/.config/nvim/after/queries/cpp/highlights.scm 或 init.lua 里的 highlight 组
vim.api.nvim_set_hl(0, "@lsp.type.formatSpecifier.cpp", { link = "@string.special" })
vim.api.nvim_set_hl(0, "@lsp.type.formatConversion.cpp", { link = "@keyword" })
vim.api.nvim_set_hl(0, "@lsp.type.formatArgument.cpp", { link = "@parameter" })
```

VS Code 需要主题里 `semanticTokenColors` 有对应项；文档里给推荐值即可。

---

## 6. 配置

放在 `.cppls.toml`，与 `[hover] enable` 同一个读取路径（`config.rs` 的 section 结构）：

```toml
[highlight]
format_strings = true    # std::format / print 家族
printf = true            # printf 家族
scanf = true             # scanf 家族
extra = ["mylib::logf"]  # 额外名字，按解析后的限定名匹配
```

* 关闭时这一步完全不跑（零成本）；
* 读取在 handler 边缘（和 `hover_of_the_program` 里 `hover.enable` 的判断同一层），scanner 不知道配置存在；
* `extra` 匹配的是**解析后的 qualified name**，避免 `logf` 撞名。

---

## 7. 正确性清单（实现时逐条对照）

```text
1. LSP 的 character 是 UTF-16 码元：一律走 encode/position_at_offset，不自己乘除；
2. 先跳 C++ 字面量自己的转义（\" \\），raw string 不跳；
3. {{ }} %% 是转义，不消耗实参；
4. 嵌套 {} 只允许一层（动态宽度/精度），内层标 DynamicWidth；
5. { 未闭合、% 在末尾、%[ 没有 ] → 画到字面量末尾，绝不越界；
6. scanf 的 %[ 要吃到 ]；%] 里的 ] 是内容；%n、%* 特殊但本阶段不判类型；
7. 相邻字面量拼接（"a" "%d"）见 §9 phase 3，phase 1 只扫单个 token（文档里写明这个限制）；
8. 宏包一层（#define LOG(f) printf(f)）→ 调用点不是字面量，跳过；
9. 只在 view.written_text() 可读时做；
10. 每文件 token 上限与既有预算共用；
11. 输出升序、不重叠（push_range 首占先得）；
12. 配置关闭时零成本，且不要缓存"关闭"状态到跨配置的地方。
```

---

## 8. 测试与度量

```text
A. scanner 单测 —— tests/format_scanner.rs（纯文本，不建 session）
   表驱动 ~40 例，断言 PieceKind + 字节区间：
     {:d}  {:>10.3f}  {:0{width}}  {{}}  {:?}  {:L}
     %d  %*d  %.2f  %1$s  %-8.2f  %[a-z]  %%  %n  %m
     R"(...)"  带 \" 的字面量  中文 fill  未闭合的 { 和 %
   失败时打印"期望/实际"的区间表。

B. handler 测 —— crates/cpp_ls/tests/（照 semantic_token 现有 fixture 的写法）
   一个 fixture（std::format + printf + scanf 各一行），断言 delta 编码后的五元组完全一致；
   再来一个"客户端只认标准类型"的用例，断言回退后的类型编号。

C. 真项目探针（只读，结果写进 docs/architecture.md 的新一节）
   · test_module 的三个文件：token 数、handler 耗时（目标：100 个字面量 < 1 ms）；
   · neovim 项目（1337 文件）：一次全量 semanticTokens 的增量；
   · 关闭配置时的耗时（应≈0）。
```

数字与结论写进 `docs/architecture.md` 的 §16（格式字符串），包括 legend 增量（+4）、每文件 token 数、
耗时。

---

## 9. 交付顺序（每步独立可验证、可回退）

```text
Phase 1  scanner(Std) + legend + LSP 接线 + A/B 测试                    ~300 行 + 测试
Phase 2  printf / scanf 文法 + 实参位置表 + 配置                         ~200 行
Phase 3  相邻字面量拼接、constexpr 值、hover（"%.2f = 定点两位小数"）、
         诊断（格式串与实参个数/类型不匹配，复用 pieces + 实参类型）
```

Phase 1 的验收：你的 `std_module_demo.cpp` / `main.cpp` 里 `std::println`、`std::format` 的参数**在编辑器里
分段着色**，且关闭配置后完全恢复原样。

---

## 10. 风险与取舍

* **自定义 token 类型**：LSP 允许，客户端在 `initialize` 里声明支持；不支持的走回退。风险只是主题不配色，
  文档给推荐映射即可。
* **认识的函数有限**：默认只认标准名，项目的包装函数通过 `extra` 加。宁可少画，不要画错。
* **性能**：scanner 是 O(字面量长度)，locator 每次调用一次 `definitions`（有缓存）。semanticTokens 本来
  就是全量一次、按需重发，这一层在实测里应当是亚毫秒；若超出，把 locator 的结果按
  `(CallExpr 的 callee, 实参下标)` 缓存一次即可。
* **不重做库的检查**：格式串是否合法由编译器/库负责，我们只画；将来做诊断时也只在"能确定"的形态上报
  （字面量 + 可解析的实参类型）。
* **不追 C++26 的 `std::format_string` 变化**：若标准库把格式串改为 `format_string<Args...>` 构造，
  locator 的"第几个实参"表仍然成立（位置没变），不受影响。
