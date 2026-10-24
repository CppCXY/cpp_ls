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

## P2：还没做

### ④ 一个 local 如果在 `local_references_of` 里没有条目，`rename` 就拒绝它

手写测试里 `int more[MAX_ITEMS];`（数组长度是宏）是索引里的事实，但 `local_references_of` **没有**它的条目。
现在的行为是"名字被分析放置了才允许改"：`symbol_at` 检查这一点，所以 `prepareRename` 和 `rename` 不会一个给框、
一个不改（`handshake.rs` 的那一半断言就是这个状态）。这是一个需要查的缺口，不是"按设计不支持"。

### ⑤ `signatureHelp` 仍未在真实项目上回答过

位置换算修好之后，12 个调用点仍然全 `null`，而 `debug` 日志里**没有** `no signature yet` —— 说明
`is_locally_ready` 那道门过了，是 `signatures_at` 在这些位置没找到调用。分析层有 21 个测试
（`tests/signature.rs`），所以是这个 handler 与真实渲染的交接处的问题，不是算法。下一步：给这个 handler 加
`CPPLS_TRACE_SIGNATURE` 探针（打印光标处的节点、`signatures_at` 的入参和结果），在一个**单行调用**上定位。

### ⑥ `diagnostic` 与批处理对同一文件结果不同

服务器对 `CodeFormat.cpp` pull 出 0 条（settled true），`edit_loop` 1 条 —— 那 1 条是已知假报
（`std::vector::erase` 没进事实），所以 0 反而对；但两条路径看到的**事实集不同**要说清楚，并用一条真报
（`else-after-return` 之类）在两边对齐。

### ⑦ 缺的功能

```text
callHierarchy            没有
typeDefinition           没有（对 `auto` 有用）
workspace/diagnostic     明确不支持，只有 pull
semanticTokens 增量      只有 full（507 token / 26 ms，可接受）
inlayHint/resolve        没有（可接受）
```

## 表（P1 之后的真实运行）

```text
feature                answered n     time   note
hover                  yes      3     21 ms  `LuaDiagnosticStyle _diagnosticStyle`
definition (member)    yes      1     2 ms   codeformat.h:56
definition (header)    yes      1     1 ms   codeformat.h:56
implementation         yes      1     4 ms   CodeFormat.cpp:31
references             yes      6     10 ms   codeformat.cpp:32
documentHighlight      yes      6     3 ms    ← 0 之前
prepareRename          yes      1     10 ms  placeholder `_diagnosticStyle`   ← 之前 null
rename                 yes      2     11 ms  codeformat.h: 1 | codeformat.cpp: 6  ← 之前 null
documentSymbol         yes      14    1 ms
workspaceSymbol        yes      512   7 ms
foldingRange           yes      39    1 ms
selectionRange         yes      5     1 ms
semanticTokens         yes      507   26 ms
inlayHint              yes      5     23 ms
documentLink           yes      5     1 ms
diagnostic             yes      0     29 ms
completion (member)    yes      2     27 ms  Editorconfig, Workspace
signatureHelp          no       0     0 ms   null for 12 call sites（P2④）
codeAction             yes      0     2 ms
rangeFormatting        yes      0     2 ms
formatting             yes      0     1 ms
```