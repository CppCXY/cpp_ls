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

### ⑧ 成员表的两个真问题（2026-10-25 下午）

**`std::string` 一旦 `bits/basic_string.h` 被 cook 就没有成员了。** libstdc++ 把类声明在 `std::__cxx11` 里
（`_GLIBCXX_BEGIN_NAMESPACE_CXX11`），而 `std::string` 的别名目标拼成 `basic_string<char>` —— cook 之后
`std::basic_string` 的 scoped 查找回答 NotDeclaredHere，成员表空、`unlisted` 里写着那个名字。真项目实测：
`std::string` 0 个成员（未 cook 时是 257）。修：scoped 拼写查不到时**按最后一段重试**（只在这一种情况下），
因为内联命名空间不是读者写出来的名字的一部分。

**索引做出来的成员表对所有"只是使用这个类"的文件是同一份。** 缓存键带调用方的路径，是因为在**被读的文件里声明**的类
要按那个文件自己的树解析；当贡献文件里没有调用方时，这份表也记在空路径下，下一个文件直接命中。

```text
std::string   从 CodeFormat.cpp 16.1 ms，从 LuaSyntaxTree.cpp **143 µs**
修之前一个 pull：12 次构建、**0 次命中**，`std::vector<LuaSyntaxNode>`（22 ms）被每个提到它的文件重建
```

模板实例化（`std::vector<LuaSyntaxNode>`）仍然是按拼写替换的（1.7 ms 然后 20 ms），新测试没有钉它。

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
#### 已修：参数解析失败的请求**得不到任何回答**

`lsp_status` 加上 `prepareCallHierarchy` 之后探针**卡住**。原因有两层：探针自己的 `position` 多套了一层
（客户端 bug），而**服务端在参数解析失败时什么都不回** —— `dispatch_request!` 写成 `if let Ok(..) = extract(..)`，
失败就掉出分支：没有响应、没有日志，日志里还写着 `dispatch: returned`。客户端只能理解为"服务器卡死了"，之后
每个请求都排在同一片沉默后面。

修法（协议本来就这么规定的）：那个分支现在回 `InvalidParams` 并带上 serde 的错因，
`handshake.rs::a_request_with_unreadable_params_is_answered_with_invalid_params` 钉住它：
`{"error":{"code":-32602,"message":"… missing field `line`"}}`。

修完之后真机三行就都答了（同一个探针，22 s 通过）：

```text
prepareCallHierarchy   yes  1  0 ms  UpdateDiagnosticStyle
incomingCalls          yes  0  3 ms  （入口函数没人调用）
outgoingCalls          yes  0  0 ms  （头文件里只有声明，函数体在 CodeFormat.cpp）
```

顺带留下两条只打慢路径的 debug 诊断：`catch_up_upto` 超过 50 ms、`apply_upto` 等 applier 或单个事件超过 50 ms
（请求在 handler 第一行之前就等在这里，以前"卡住"和"慢"分不开）。

#### 已修：pump 空闲时每秒一次空 slice

同一个请求在**分析层**是毫秒级（`tests/call_hierarchy_real.rs`，真项目）：

```text
prepare            63 µs   （UpdateCodeStyle）
incomingCalls      4.5 ms  （一个调用者：C API 里的同名自由函数 UpdateCodeStyle —— 名字对，因为那确实是一个同名函数）
outgoingCalls      9 µs    （头文件里的声明没有函数体）
outgoing（定义处）  16 ms   （OpenFile ×2、emplace_back ×1）
```

所以卡的不是这三个问题，而是 `AnalysisState::prepare` → `want_cooked_reading` 之后 **pump 的"没有进展"循环**：它占着
写锁一遍遍重试，请求拿不到读锁。这是一个**饥饿 bug**，与 call hierarchy 无关，任何在渲染被挤掉之后调用 `prepare`
的请求都会踩到（`hover` 之前也会，只是它现在不碰渲染了）。

**定位（同一轮内）**：给 handler 首行加了 `log::debug!` 之后重跑，日志里**只有请求到达、没有 handler 的第一行** ——
所以卡的不是 handler，而是它之前的 `catch_up_upto`（`lsp_server` 收下请求之后、`RequestManager::task` 里先等
inbox 的 `applying` 锁）。同一时间 pump 每秒一次空 slice（`pending == 0` 时它也会被 `IDLE_WAIT` 唤醒），
每 8 次打一条 no-progress 警告。

pump 尾巴原来是 `wait_for_work(IDLE_WAIT).await` 之后**不管结果都走一圈**，于是**超时也跑一个完整 slice**
（parse wave、commit、claim cooking、pending 计数、状态栏刷新，全是空的）。修法：超时（`false`）就 `continue`
继续等 —— 到那里时 `pending` 一定为 0（`pending > 0` 的分支已经先 `continue` 了），所以"超时"只意味着"再等"。
另外 "no progress" 计数现在只统计**本来有活却没干成**的 slice（一次 `prepare` 唤醒一个已经就绪的文件不再算）。

新增探针 `tests/idle_probe.rs`（起真服务器、初始化、等索引排空、然后 10 秒不发任何请求，数日志）：

```text
修之前：10 秒空闲 = 16 个空 slice、**2 条 no-progress 警告**
修之后：10 秒空闲 = 0 个 slice、**0 条警告**
```

`lsp_status`（80 次 pull + 29 行功能表）和 `loop_probe`（第一轮 10 次 pull 2 次 cook、第二轮 0 次）都照旧。
### ⑩ 「预处理器有根本性问题」的回执：不是预处理器，是**eager 索引的写法**

截图里 `auto &config = _configs.emplace_back(` 跳到 `stl_bvector.h:1525`，看到的是

```cpp
#if __cplusplus > 201402L
    _GLIBCXX20_CONSTEXPR
    reference
#else
    void
#endif
    emplace_back(_Args&&... __args)
```

而 `emplace_back` 在我们的事实里 `returns: Some("void")` —— **`#else` 那一支**，在 C++20 工程里是错的（应为 `reference`）。

**预处理器本身是对的**。把同一个头文件渲染出来（`rendering_probe --dump`），那一行是：

```text
template<typename... _Args> constexpr reference emplace_back(_Args&&... __args) { … }
```

不活跃的分支被删掉了，`reference` 保留；渲染里 294 条声明、raw 439 条、双方 0 个错误节点。

问题在**读的是文件自身文本**：类型是从 `DeclSpecifierSeq` 里"最近的类型名"读出来的，而那个节点的文本就是
`_GLIBCXX20_CONSTEXPR\n\treference\n#else\n\tvoid\n#endif` —— 于是拿到了 `#else` 的分支。

**测量**（新探针 `examples/facts_vs_rendering.rs`，按名字+作用域把两种读法对齐）：

```text
stl_bvector.h   439 条事实，4 条与渲染的返回类型不一致
stl_vector.h    503 条，4 条
basic_string.h 1130 条，4 条
合计            1589 条匹配，**12** 条矛盾
```

**已做的修法（不是 workaround）**：一份声明如果它的说明符序列被指令打断，那么**文件自身文本没有单一的返回类型**，
事实就不写类型 —— `returns: None`/`type_of: None`。拿着渲染的消费者有真值；只拿摘要的消费者被告知真相，而不是
一个分支。测完：`emplace_back`/`insert` 的 `returns` 从 `Some("void")` 变成 `None`；三头文件里矛盾 12 → **10**、
另有 **7** 条改为"不表态"（`facts_vs_rendering` 现在分开计这两个数）。

`tests/semantic.rs::a_return_type_a_directive_splits_is_not_named` 钉住两半：分割的 `make` → `None`，不分割的
`plain` → `Some("int")`。

**第二步（已完成）：需要精确类型的消费者去问渲染。** 新 `Session::fact_from_the_rendering(path, name, scope)`：
`rendering_of`（cook 缓存，一次 hash+lookup）→ 对渲染文本 `summarize` → 按名字+作用域取那条声明。hover 在
`split_by_a_directive` 为真时走这条路，于是 `emplace_back` 显示 `reference`：

```text
written:      returns None, split_by_a_directive true
rendering:    returns Some("reference")
             第一次 24.1 ms（cook 渲染 + 摘要），第二次 **205.6 µs**
```

**这两层缓存都是这一轮补上的**：`rendering_summaries`（按 `(path, 渲染文本内容 hash)` 的 LRU，权重 = 声明数+宏数，
16 条 / 40 000）让第二次不必再 parse 渲染文本；而 `fact_from_the_rendering` 自己**建的渲染会存回** `renderings`
缓存（它以前只查不存，所以第二次仍要重建一次渲染，4.2 ms）。修之前是 20.2 → 19.1 ms，修之后 24.1 → 0.2 ms。

**还没做**：剩下 10 条矛盾（`operator bool`、`_M_data_ptr`、`basic_string.h` 的 4 个 `__s`）是别的形状（指令在声明
**之前**、或渲染把整条声明删掉）。
### ⑪ 剩下 6 条：三个不同的形状，全部归零

```text
operator bool   written "_GLIBCXX20_CONSTEXPR" → rendering "void"    ← 转换运算符的返回类型被当成"名字前的最后一个标识符"
insert ×4       written "void"                 → rendering "iterator" ← 一条声明写在一个分支里，另一分支的声明也是答案
_M_data_ptr     written "value_type *"         → rendering "_Up *"    ← 同上
```

**转换运算符的返回类型就是它的名字。** `_GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR operator bool()` 里，写读法把宏名
`_GLIBCXX20_CONSTEXPR` 当成了返回类型 —— 它不是类型、任何查找都解析不了。规则改成按拼写取：`operator bool` →
`bool`（`operator[]`/`operator+` 没有空格，不匹配这条规则，它们的返回类型是真说明符）。两边读法现在都是 `bool`。

**死分支的声明不是答案。** `insert`/`_M_data_ptr` 在 `#if __cplusplus >= 201103L … #else … #endif` 的两边各写一份，
两份都成了答案，于是 hover/补全可能给出编译器根本不读的那一条。查找现在对每个候选问
`visibility_at(self, file, fact.guard, offset)`，`Inactive` 丢弃、`Unknown` 保留（判不了的条件不能变成"这个名字没声明"）。

**代价，用 A/B 量过**：

```text
不做缓存         diagnostic 28 → **71 ms**，80 次 pull 的落定时间 23.8 → 28.2 s
加 verdict 记忆    diagnostic **29 ms**，落定 24.2 s（与没加过滤器的 23.8 s 在噪声内）
```

记忆是 `guard_answers: (file, offset, region) → Visibility`，上限 16 384 条，文件一改（`forget_member_lists` 的每个
调用点）就丢。verdict 本身按 (file, offset, region) 是稳定的：同一个事实在每次 pull 被问的都是同一件事。

`tests/rendering_real.rs::a_dead_branch_is_not_an_answer` 钉住它：`insert` 的五个答案全是 `iterator`，`_M_data_ptr`
不再有 `value_type *`。`facts_vs_rendering` 三个头文件现在是 **0 条矛盾**（267 条可比较的事实，195 条是分支内的声明，
单独计数）。
### ⑫ 「预处理器正确，为什么 parser 还看得见宏？」

因为**主读法不是预处理之后的文本**，而是文件自身文本 —— 这是设计选择，不是 bug：

* 一个头文件**没有统一的预处理结果**：`#if __cplusplus`、`#ifdef _GLIBCXX_USE_CXX11_ABI`、`_GLIBCXX_DEBUG` 都取决于
  **包含它的那个 TU**。所以"按文件建索引"只能存 raw 读法；渲染是**按 TU/按需**的产物（`rendering_of`）。
* 于是 parser 在 raw 读法里看到的 `_GLIBCXX20_CONSTEXPR`、`__CHAR_BIT__`、`_GLIBCXX_NODISCARD` 就是**普通标识符**：
  它会把它们当说明符、当类型、甚至在声明开头把它们当成一条声明。

**这漏出来多少，量了**（`facts_vs_rendering`，三个头文件，2072 条 raw 事实）：

```text
与渲染有对应、可比的事实         267 条   返回类型/type_of 矛盾 **0** 条
渲染里根本没有对应物的事实        483 条   —— 其中 **462** 条在条件分支里（编译器不读，查找已按 visibility 过滤）
                                        —— **21** 条是 Unconditional 的"孤儿"
```

那 21 条孤儿就是"parser 看见宏"的真身，样本直接指向形状：

```text
_GLIBCXX_NODISCARD        kind Variable   ← 宏调用本身被当成一条变量声明
__CHAR_BIT__              kind Type       ← 预定义宏被当成一个类型
_Bit_iterator / _Bit_const_iterator / _Bit_iterator_base   kind Function/Variable
                          ← 说明符里的宏让 parser 把**类型**当成了声明的名字（真正该是 _M_start 之类）
```

也就是说：**约 1%（21/2072）的 raw 事实是宏文本造成的**，其中一半是"名字 = 一个宏"，一半是"名字错成了类型名"。
预处理器正确并不影响这条路径 —— 它压根没在这条路径上跑。

下一个候选修法（按代价排序）：① 查找时丢掉"名字是闭包里某个宏"的事实（`ProjectIndex::macros()` 已经在手里，
`_GLIBCXX_NODISCARD`/`__CHAR_BIT__` 这类直接消失）；② 说明符序列里的宏调用要让 parser 继续往后找声明符
（`_Bit_iterator` 这类改名字的），这要动 grammar 的 specifier/declarator 规则，风险和收益都比 ① 大。