# LSP 功能自测：现状（2026-10-25 凌晨）

`crates/cpp_ls/tests/lsp_status.rs`：起真服务器、开真项目（`E:\EmmyLuaCodeStyle`）的
`CodeFormatCLib/src/CodeFormat.cpp` 和 `codeformat.h`，等索引排空（80 次 pull、22 s），然后每个功能问一个
**有形状的**问题（条数、落在哪个文件哪一行），打印一行。

```text
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
cargo test --release -p cpp_ls --test lsp_status -- --ignored --nocapture
```

## 表（最近一次真实运行）

```text
feature                answered n     time   note
hover                  yes      3     21 ms  `_diagnosticStyle = LuaDiagnosticStyle();` → `LuaDiagnosticStyle …`
definition (member)    yes      1     2 ms   codeformat.h:56
typeDefinition         yes      1     1 ms   LuaDiagnosticStyle.h:10      ← 类型，不是成员
typeDefinition (auto)  yes      2     2 ms   `auto result = …` → stringfwd.h:79（推断出的 std::string）
definition (header)    yes      1     0 ms   codeformat.h:56
implementation         yes      1     4 ms   CodeFormat.cpp:31
references             yes      6     11 ms  codeformat.cpp:32
documentHighlight      yes      6     3 ms
prepareRename          yes      1     12 ms  placeholder `_diagnosticStyle`
rename                 yes      2     12 ms  codeformat.h: 1 | codeformat.cpp: 6
documentSymbol         yes      14    2 ms   GetInstance, CodeFormat, UpdateCodeStyle
workspaceSymbol        yes      512   8 ms
foldingRange           yes      39    1 ms
selectionRange         yes      5     1 ms   5 nested range(s)
semanticTokens         yes      516   29 ms
inlayHint              yes      5     46 ms
documentLink           yes      5     2 ms
diagnostic             yes      0     47 ms
completion (member)    yes      2     21 ms  Editorconfig, Workspace
signatureHelp          yes      1     0 ms   at 82:17 → SetupStyle(LuaStyle &style, const FormattingOptions &options)
codeAction             yes      0     139 ms
rangeFormatting        yes      0     3 ms
formatting             yes      0     4 ms
```

## 探针

```text
tests/lsp_status.rs           上表本身，可重跑（22 s）
tests/implementation_real.rs  真实项目：头文件里的声明 → 定义它的 .cpp
tests/signature_real.rs       真实项目：一个调用点回答一个签名（含 parameter_list 的来源）
tests/members_real.rs         真实项目：类的成员表（std::string 257、std::vector 285 含 erase、签名）
examples/parse_loss_probe     每个文件：没有参数表的函数事实、ErrorNode；--show 看形状，--braces 找吞掉 `{` 的节点
examples/facts_probe          一个文件的事实（名字、kind、scope、参数表）
```

## P1（已完成）

### ① `rename` / `prepareRename` 支持普通符号 ✅

宏的路由不变（`macro_at` 先问），没有宏时 `symbol_at`：拼写和 range 取自**文件自身文本**，`definitions` 只用来确认
"这是个符号"（它的回答可能在头文件里，而光标不在那儿）。编辑就是引用那条规则
（`references::name_level_offsets`，抽出来给 rename 用），所以"一个类的成员不是另一个类的"由构造保证。
`prepareRename` 回答 `RangeWithPlaceholder`，框里直接有名字。

### ② `documentHighlight` 支持成员 ✅

local 的路径不动；local 没有答案时走 `symbol_highlights`——同一个 `name_level_offsets`，限定当前文件，声明是
`Write`、其余 `Read`。真项目上 0 → **6**。

### ③ 位置换算：先文件、再渲染 ✅

`hover`、`signatureHelp`、`codeAction`、`documentHighlight`、`selectionRange` 以前把客户端位置**通过渲染**换算，
而渲染里指令已展开、行被合并，名字落在别处 —— 这就是 `implementation` 在真实项目上回答 `null` 的原因（修好后第一个
回答是 `CodeFormat.cpp:31`）。现在一律 `written = view_of_the_file` → `file_offset_at_position` → 需要渲染时再
`reading_offset_of(FileOffset)`。

## P2（已完成）

### ④ `signatureHelp` 在真实项目上回答了 ✅

三个环节，每个都是"零答案"链条上的一环：

```text
位置        光标必须落在**参数表里面**（在函数名上，"没有调用"就是正确回答）
参数表      索引对"本文件用到的名字"回答本文件的**定义事实**（own file wins），而定义事实的 parameter_list 是
            None —— 类外定义的名字写在说明符序列里，fact_for 通过 declarator 读不到
候选        parameter_list_of 在同一个文件里也找不到 declarator，sources 为空，循环零次，**静默丢弃**
```

修：`qualified_declarations` 自己从声明头部读参数表；`signatures_at` 在 `sources` 为空而事实带 `parameter_list`
时用事实造标签。

### ⑤ local 的改名 ✅

`local_references_of` 只把**使用**记在声明名下，所以"声明了但没人用"的 local 没有条目。改名现在以光标下的名字开头
（可能没有使用），且只在本文件内。`handshake.rs` 断言的就是这个形状。

### ⑥ 三个解析器/事实层的 bug（诊断 9 → 0）

**注释吃掉类的右花括号。** `struct S { int x; // note\n};` 的 `}` 被收成 ErrorNode，类体越过自己的结尾继续。
libstdc++ 的 `struct _Alloc_hider : allocator_type // TODO check __is_final` 正好踩中，于是 `basic_string` 之后
一千多个成员落到嵌套类名下：

```text
std::string / std::basic_string   27 → 257 member(s)，size/erase/begin 都在
CodeFormat.cpp 诊断                9 → 0
semantic tokens                    507 → 516
```

**`#if` 夹在返回类型和名字之间。** 未 cook 的读数里 `iterator` 后面跟着 `#`，"这个名字后面还有没有声明符"回答
"没有"，于是 `iterator` 变成声明符，`std::vector::erase` 一个事实都没有。修：那个问题**看穿指令**（按逻辑行跳过）。

**限定名调用被当成外来函数的定义。** `std::iter_swap(a, b);` 被读成声明（类型 `std::iter_swap` + 声明符 `(a, b)`），
作用域遍历那条为类外定义写的分支把函数本身绑进了 `std`。定义不可能嵌在 body 里 → "在不在 body 里"就是判据。

```text
libstdc++ 579 个头文件   56 734 → 55 549 事实（1 185 个幽灵事实消失）
                        15 253 → 14 609 函数事实
                        没有参数表的函数 1 937 → 312 → **18**（0.12%）
```

**还差 18 个**：`fs_path.h` 的 12 个转换运算符、`alloc_traits.h` 的 6 个 concept（`requires` 里夹 `#if`）。最小夹具
复现不出来，依赖上下文 —— 见 P3。

### ⑦ `typeDefinition`（新增）

```text
definition (member)   codeformat.h:56          ← `_diagnosticStyle` 这个成员声明在哪
typeDefinition        LuaDiagnosticStyle.h:10  ← 它的**类型**声明在哪
```

```text
变量/参数/成员    声明的类型的声明     Widget w;         → struct Widget
成员访问          成员的声明类型       w.size（返回 int）→ 空（int 没有声明）
类名              它自己的声明         std::string s;    → 那个类
```

## P3（未做）

```text
那 18 条                 见上（上下文相关，最小夹具复现不出来）
callHierarchy            没有
workspace/diagnostic     明确不支持，只有 pull
semanticTokens 增量      只有 full（516 token / 25 ms，可接受）
inlayHint/resolve        没有（可接受）
```

已完成（原来在 P3 里）：

```text
typeDefinition 的 auto   推断在**渲染**里做（文件自身文本回答 Known::No），所以第一个读数没答案时用
                         `Session::rendering_of` 自己建一份渲染（&self、不排 cook、不受渲染缓存影响）再问一次
                         → 真项目 `auto result = f.GetFormatResult(t);` → stringfwd.h:79，2 ms
构造函数的签名            `LuaParser p(file, std::move(...))` 是**直接初始化**，语法上是一个声明（声明符里带参数表），
                         不是调用 —— 以前连候选都没有。新 `directly_initialized_at` 认出这个形状（有参数表、
                         没有函数体），类名取说明符里最后一个名字，答案来自索引里的构造函数事实
                         → `LuaParser(std::shared_ptr<LuaSource> luaFile, std::vector<LuaToken> &&tokens)`
```

## 测量时的两个陷阱

```text
① 换过文件之后 cargo 不会重建
   Copy-Item / git checkout 恢复的文件保留**旧的** mtime，cargo 认为没变，测试二进制报的是上一个版本的行为。
   这一轮因此得到过三次互相矛盾的结论。换完文件 touch 一下，或者 cargo clean -p <crate>。

② 位置算错了，结论也会跟着错
   CRLF、UTF-16 列、渲染坐标 —— `implementation` 的 null、`signatureHelp` 的 null、探针自己的"0 个签名"，
   三件事各有一个位置错误。凡是"这个功能不工作"的结论，先打印**光标处的那段文本**确认它真的是你想测的地方。
```

这一轮所有结论都以 `cargo clean -p` 之后重测的数字为准：`std::string` 257 个成员、`std::vector` 285 个（含 `erase`）、
150 834 个事实、0 条诊断、libstdc++ 18 个函数没有参数表。
## ⑥ 截图里的六个问题（2026-10-25 中午）

```text
① 智能指针的 `.` 补全   shared_ptr 自己的成员（operator= / element_type / weak_type）
                        → 现在跟随 `operator->`：pointee 的成员，并把 `.` 替换成 `->`
② 分离定义的成员 hover  "2 declarations" + "which one a call means is decided by the arguments"（对数据成员）
                        → 按光标所在类收窄（引用/定义同一条规则）：真项目上 3 → 2（一个声明）
③ 类型的 hover          "3 declarations"（类 + 两个构造函数被当成重载）
                        → 有一个 Type 事实时就回答它：`class LuaDiagnosticStyle`
④ 头文件 go to impl     GetErrors 没有结果
                        → 作用域遍历给类外定义的 fact 是 `scope: None` + **声明符的** range；限定名那一遍现在
                          **收编**它并补上 scope 和声明的 range（"是不是定义"问的就是这段文本结尾是 `;` 还是 `}`）
                          → LuaLexer.h 的 GetErrors → LuaLexer.cpp:73
⑤ CPU / cook 循环       pull 循环是健康的（loop_probe：第一轮 10 次 pull 2 次 cook，第二轮 **0**；
                        整个 run 88 次 cook 全在 1273 个文件的首次索引里）
⑥ 分离定义的 cpp 里没有 hover
                        → hover 以前要**渲染**（`session.view`），而渲染缓存只有 8 条：hover 把正在看的文件自己
                          挤出去，于是这一次回答 null、同时排一次 cook（日志里每次 hover 一条
                          `cook slice: asked 1, rendered 1`）。现在 hover 从**文件自身文本**回答，不碰渲染；
                          文档注释那条路（`documentation_of_a_fact`）同理 —— 它是唯一还在排 cook 的地方
                          → 真项目上 hover 25 ms → 11 ms，四个位置全部有答案
```

⑤ 剩下的 CPU 是**首次索引 1273 个文件**的固有成本，不是循环；要再压就得动索引本身（那是另一个话题）。
② 和 ⑥ 的截图来自同一份日志，现在 hover 不再触发 cook，日志里也不该再有每 hover 一条的 cook。