# LSP 功能自测：哪些能用，哪些要改（2026-10-24）

这份表来自 `crates/cpp_ls/tests/lsp_status.rs`：起真服务器、开真项目（`E:\EmmyLuaCodeStyle`）的
`CodeFormatCLib/src/CodeFormat.cpp` 和 `codeformat.h`，等索引排空（80 次 pull、22 s），然后每个功能问一个
**有形状的**问题（条数、落在哪个文件哪一行），打印一行。

```text
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
cargo test --release -p cpp_ls --test lsp_status -- --ignored --nocapture
```

## 表（一次真实运行，2026-10-24 21:2x）

```text
feature                answered n     time   note
hover                  yes      3     22 ms  `_diagnosticStyle = LuaDiagnosticStyle();` → `LuaDiagnosticStyle _diagnosticStyle`
definition (member)    yes      1     2 ms   codeformat.h:56
definition (header)    yes      1     1 ms   codeformat.h:56
implementation         yes      1     5 ms   CodeFormat.cpp:31
references             yes      6     13 ms   codeformat.cpp:32
documentHighlight      yes      0     1 ms   （成员不是"local"，见下）
prepareRename          no       0     10 ms  null
rename                 no       0     10 ms  null
documentSymbol         yes      14    1 ms   GetInstance, CodeFormat, UpdateCodeStyle
workspaceSymbol        yes      512   7 ms
foldingRange           yes      39    1 ms
selectionRange         yes      5     1 ms   5 nested range(s)
semanticTokens         yes      507   31 ms
inlayHint              yes      5     25 ms
documentLink           yes      5     2 ms
diagnostic             yes      0     33 ms
completion (member)    yes      2     27 ms   Editorconfig, Workspace
signatureHelp          no       0     0 ms   null for the first 8 call sites
codeAction             yes      0     1 ms
rangeFormatting        yes      0     2 ms
formatting             yes      0     2 ms
```

## 合理（能用，且有测试或这次实测支撑）

```text
hover                 类型 + 名字，答案里带 LuaDiagnosticStyle；22 ms
definition            成员只给本类那一条（今天的收窄规则），头文件里的声明也算一条
implementation        **今天修好的**：头文件声明 → CodeFormat.cpp:18/31（见"这次发现的缺陷"）
references            6 条、跨文件、含头文件声明；按类过滤 + 接收者类型过滤
documentSymbol        14 个顶层符号，1 ms
workspaceSymbol       512 条（"Format" 前缀），7 ms
foldingRange          39 段，1 ms
selectionRange        5 层嵌套，1 ms
semanticTokens        507 个 token，31 ms（同步、全量）
inlayHint             5 条，25 ms
documentLink          5 条 include，2 ms
diagnostic            pull 模式，0 条（见下"数字对不上"一节）
completion            成员补全 2 条（`Editorconfig`/`Workspace`）——`_configs` 的成员，正确
formatting / rangeFormatting / codeAction   都回答（空数组合法：这份文件已经格式化过、区间里没有诊断）
```

## P1：已完成（2026-10-24 晚）

### ① `rename` / `prepareRename` 支持普通符号 ✅

宏的路由不变（`macro_at` 先问），没有宏时走 `symbol_at`：**拼写和 range 取自文件自身文本**
（`name_at_including_directives`），`definitions` 只用来确认"这是个符号"——它的回答可能在头文件里，而光标不
在那儿。编辑用的就是引用那条规则（`references::name_level_offsets`，抽出来给 rename 用，位置报告和字节偏移
两个视图不会各算一遍），所以"一个类的成员不是另一个类的"由构造保证。

判据（`a_symbol::is_renamed_in_its_own_class_and_the_declaration_follows`）：两个类各有 `_value`、各用一次
→ 2 处编辑（头文件声明 + 本类用法），`Second` 的声明和用法都不在里面。
真实项目上：`prepareRename` 回答 `_diagnosticStyle` 的 range，`rename` 回答 `codeformat.h: 1 | codeformat.cpp: 6`。

### ② `documentHighlight` 支持成员 ✅

local 的路径不变；local 没有答案时走 `symbol_highlights`——同一个 `name_level_offsets`，限定在**当前文件**，
声明那一条是 `Write`、其余是 `Read`。真实项目上从 `0` 变成 **6**。

判据（`a_symbol::is_highlighted_in_this_file_and_another_classs_is_not`）。

### ③ 位置换算：五个 handler 改成"先文件、再渲染" ✅

`hover`、`signatureHelp`、`codeAction`、`documentHighlight`、`selectionRange` 以前
`session.view(&path)?`（渲染）配 `offset_at_position(&view, position)`；现在一律

```text
written  = session.view_of_the_file(&path)      文件自身的文本
offset_f = file_offset_at_position(&written, position)
offset_r = rendering.reading_offset_of(FileOffset::new(offset_f))   需要渲染时
```

`documentHighlight` 和 `selectionRange` 两步都留在文件坐标里（它们的查询本来就是文件坐标的）。

## P2：这一轮做的（2026-10-25 凌晨）

### ④ `signatureHelp` 在真实项目上回答了 ✅

三件事，每一件都是"零答案"链条上的一环：

1. **位置**：光标必须落在**参数表里面**——`signatures_at` 的文档就是这样写的。探针和第一版测试都把光标放在 `(` 或
   函数名上，那里的正确回答就是"没有调用"。
2. **`parameter_list` 缺失**：索引里 `definitions("SetupStyle")` 回答的是 `.cpp` 里的**定义事实**（"own file
   wins"），而它的 `parameter_list` 是 `None` —— 因为 `fact_for` 通过作用域遍历的 declarator 读参数表，而类外定义的
   **名字写在说明符序列里**，declarator 里没有名字，遍历取不到。修：`qualified_declarations` 自己从声明头部读参数表，
   `build` 把它补进事实。
3. **候选被静默丢弃**：`signatures_at` 里 `parameter_list_of(declared_in, offset)` 在同一个文件里也找不到 declarator，
   `sources` 为空 → `for` 循环零次。修：`sources` 为空而事实带 `parameter_list` 时，用事实造标签（和另一个文件那条
   回退路径同一个形状）。

实测（`tests/signature_real.rs`）：`SetupStyle(style, options);` → `SetupStyle(LuaStyle &style, const
FormattingOptions &options)`；LSP 探针同一行：`signatureHelp yes 1 at 82:17 → SetupStyle(...)`。

### ⑤ 一个 local 的改名：真正的缺口是"只有被用到的声明才有条目"

`local_references_of` 把**使用**记在声明名下，声明本身被排除；所以"声明了但没人用"的 local 没有条目。这不是"宏做
数组长度"的问题（`int more[N];` 在 `tests/scopes.rs` 里是正常的）。改名现在这样处理：

```text
local → 只在本文件内改（跨文件会改到别的文件的同名 local）
         列表以光标下的名字开头，再加上条目里的使用（可能没有）
```

`handshake.rs` 断言的就是这个形状：`more`（声明，未使用）→ 1 处编辑，在本文件。

### ⑥ `diagnostic` 现在报 9 条，其中一条是假报

`CodeFormat.cpp` pull 出 9 条，第一条是 `"result" has no member "size"`。查过源码：

```cpp
auto result = f.GetFormatResult(t);     // FormatBuilder::GetFormatResult 返回 std::string
auto length = result.size();            // 于是 size() 是对的
```

`GetFormatResult` 是**基类里的 virtual**（`FormatBuilder.h:17`），所以这是 `auto` 从"基类里的虚函数调用"推导失败。
和 `_configs has no member erase` 是同一类。这是下一轮 P2 的入口。

## P3

```text
callHierarchy            没有
typeDefinition           没有（对 `auto` 有用）
workspace/diagnostic     明确不支持，只有 pull
semanticTokens 增量      只有 full（507 token / 26 ms，可接受）
inlayHint/resolve        没有（可接受）
构造函数的签名            `LuaParser p(file, std::move(...))` 仍然 0 个签名：名字解析到类，构造函数的列表要
                          通过 `constructors_of` 读类自己的文件，而那个头文件没有渲染。下一轮。
```

## 表（这一轮之后的真实运行）

```text
feature                answered n     time   note
hover                  yes      3     20 ms  `LuaDiagnosticStyle _diagnosticStyle`
definition (member)    yes      1     2 ms   codeformat.h:56
definition (header)    yes      1     1 ms   codeformat.h:56
implementation         yes      1     5 ms   CodeFormat.cpp:31
references             yes      6     10 ms  codeformat.cpp:32
documentHighlight      yes      6     3 ms
prepareRename          yes      1     10 ms  placeholder `_diagnosticStyle`
rename                 yes      2     11 ms  codeformat.h: 1 | codeformat.cpp: 6
documentSymbol         yes      14    1 ms
workspaceSymbol        yes      512   6 ms
foldingRange           yes      39    1 ms
selectionRange         yes      5     1 ms
semanticTokens         yes      507   26 ms
inlayHint              yes      5     23 ms
documentLink           yes      5     2 ms
diagnostic             yes      9     32 ms  `"result" has no member "size"`（假报，见 ⑥）
completion (member)    yes      2     28 ms  Editorconfig, Workspace
signatureHelp          yes      1     0 ms   at 82:17 → SetupStyle(LuaStyle &style, …)   ← 之前 null
codeAction             yes      0     2 ms
rangeFormatting        yes      0     9 ms
formatting             yes      0     1 ms
```

## 探针与环境变量

这一轮把临时加的 `std::env::var` 都清了：

```text
CPPLS_TRACE_QUALIFIED   删掉（库里的 eprintln! 留在热路径上）
CPPLS_PRINT_LANGUAGE    删掉（`__cplusplus` 已经确认在宏表里，结论在 docs/semantics-todo.md）
CPPLS_PRINT_LOOKUP      变成 probes.rs 里的 switch：CPPLS_TRACE_ANSWERS
CPPLS_STATUS_LOG        删掉（探针固定用 debug，和 real_project.rs 一样）
CPPLS_REAL_PROJECT      留着：测试夹具的路径，和 real_project.rs 同一种用法
```